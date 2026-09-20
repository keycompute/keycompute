// Actual built-WASM regression using a fresh browser and loopback-only API mocks.
// KC_WEB_ROOT: Dioxus web/public output. KC_PLAYWRIGHT_MODULE: optional module URL.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.KC_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(process.env.KC_WEB_ROOT || 'target/dx/web/debug/web/public');
const output = path.resolve(process.env.KC_BROWSER_OUTPUT || 'node-ingress-browser-results');
await fs.mkdir(output, { recursive: true });
const server = http.createServer(async (req, res) => {
  try {
    const pathname = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    let file = path.resolve(root, '.' + pathname);
    if (file !== root && !file.startsWith(root + path.sep)) throw Error('invalid asset path');
    let body;
    try { body = await fs.readFile(file); }
    catch { file = path.join(root, 'index.html'); body = await fs.readFile(file); }
    const types = { '.wasm': 'application/wasm', '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html', '.svg': 'image/svg+xml', '.png': 'image/png' };
    res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
    res.end(body);
  } catch { res.writeHead(500); res.end(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const calls = [], errors = [], unknown = [], checks = [];
const check = name => { checks.push(name); console.log('PASS', name); };
const stamp = '2026-09-20T00:00:00Z';
try {
  // Isolated fixtures only: no existing browser profile or real account is used.
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.addInitScript(() => localStorage.setItem('keyc_access_token', 'mock-node-test'));
  const page = await context.newPage();
  page.on('pageerror', e => errors.push(e.message));
  await page.route('**/*', async route => {
    const req = route.request(), u = new URL(req.url()), p = u.pathname;
    if (u.origin !== base) return route.abort();
    if (!/^\/(api|v1|pt|nt)\//.test(p)) return route.continue();
    calls.push(p);
    const paged = key => ({ [key]: [], total: 0, page: 1, page_size: 20, total_pages: 0 });
    let body;
    if (p === '/api/v1/settings/public') body = { site_name: 'KeyCompute', distribution_enabled: true };
    else if (p === '/api/v1/me') body = { id: '11111111-1111-4111-8111-111111111111', tenant_id: '22222222-2222-4222-8222-222222222222', email: 'test@example.invalid', name: 'Test Admin', role: 'admin', created_at: stamp };
    else if (p === '/api/v1/keys') body = paged('keys');
    else if (p === '/api/v1/admin/node-gateway/overview') body = { enabled: true, node_stats: { total: 0, online: 0, offline: 0, excluded: 0 }, task_stats: { total: 0, queued: 0, leased: 0, succeeded: 0, failed: 0, expired: 0 } };
    else if (p === '/api/v1/admin/node-gateway/nodes') body = paged('nodes');
    else if (p === '/api/v1/admin/node-gateway/tasks') body = paged('tasks');
    else if (p === '/api/v1/admin/node-gateway/tokens/pending') body = paged('tokens');
    else if (['/v1/models', '/pt/v1/models', '/nt/v1/models'].includes(p)) {
      body = { object: 'list', data: [{ id: 'gemma3:270m', object: 'model', created: 1, owned_by: p.startsWith('/nt/') ? 'node' : 'openai' }] };
    } else {
      unknown.push(p);
      return route.fulfill({ status: 404, contentType: 'application/json', body: '{}' });
    }
    await route.fulfill({ status: 200, contentType: 'application/json', headers: { 'Cache-Control': 'private, no-store' }, body: JSON.stringify(body) });
  });
  await page.goto(base + '/api-keys');
  const guide = page.locator('.kc-mode-workflow');
  await guide.locator('button').first().click();
  await guide.locator('.kc-api-example').waitFor();
  const modes = guide.locator('[role="group"] button');
  for (const [index, family] of [[2, '/nt/v1'], [1, '/pt/v1'], [0, '/v1'], [2, '/nt/v1']]) {
    await modes.nth(index).click();
    await page.waitForFunction(family => {
      const code = document.querySelector('.kc-mode-workflow .kc-api-example');
      return code && code.textContent.includes(family + '/chat/completions');
    }, family);
    const example = await guide.locator('.kc-api-example').innerText();
    assert.match(example, /gemma3:270m/);
    assert.ok(!example.includes('node:gemma3:270m'));
    assert.ok(calls.includes(family + '/models'));
    if (index !== 0) assert.equal(await guide.locator('select').first().locator('option').count(), 1);
  }
  check('invocation_examples_and_model_discovery_follow_each_url_family');
  check('node_model_ids_remain_raw_and_no_unsupported_protocol_is_offered');
  await page.goto(base + '/admin/upstreams/nodes');
  await page.locator('.node-gateway-dispatch-guide').waitFor();
  assert.equal(await page.locator('.upstream-tab').count(), 3);
  assert.equal(await page.locator('.upstream-tab.active').getAttribute('href'), '/admin/upstreams/nodes');
  const nodeGuide = page.locator('.node-gateway-dispatch-guide');
  const copyable = await nodeGuide.locator('.kc-api-example').innerText();
  assert.match(copyable, /\/nt\/v1\/chat\/completions/);
  assert.match(copyable, /"model":"gemma3:270m"/);
  assert.ok(!copyable.includes('node:gemma3'));
  check('node_admin_tab_and_copyable_example_use_explicit_nt_ingress');
  await page.reload();
  await page.locator('.node-gateway-dispatch-guide').waitFor();
  assert.equal(await page.locator('.upstream-tab.active').getAttribute('href'), '/admin/upstreams/nodes');
  check('reload_preserves_node_management_tab');
  await page.screenshot({ path: path.join(output, 'node-desktop.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: path.join(output, 'node-mobile.png'), fullPage: true });
  assert.deepEqual(errors, []);
  assert.deepEqual(unknown, []);
  check('no_unhandled_browser_error_or_unexpected_api_request');
  await fs.writeFile(path.join(output, 'result.json'), JSON.stringify({ passed: true, checks, calls }, null, 2));
} catch (error) {
  await fs.writeFile(path.join(output, 'result.json'), JSON.stringify({ passed: false, error: String(error), checks, errors, unknown, calls }, null, 2));
  throw error;
} finally {
  await browser.close();
  await new Promise(resolve => server.close(resolve));
}
