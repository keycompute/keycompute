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
const calls = [], requests = [], errors = [], unknown = [], checks = [];
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
    requests.push(p+u.search);
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
      body = { object: 'list', data: [{ id: u.searchParams.get('stream')==='true' ? 'stream/gemma:tag' : 'gemma3:270m', object: 'model', created: 1, owned_by: p.startsWith('/nt/') ? 'node' : 'openai' }] };
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
    assert.equal(await guide.locator('select').first().locator('option').count(), 3);
  }
  check('invocation_examples_and_model_discovery_follow_each_url_family');
  check('node_model_ids_remain_raw_and_native_protocol_choices_are_available');
  await guide.locator('input[type="checkbox"]').check();
  await page.waitForFunction(()=>document.querySelector('.kc-api-example')?.textContent.includes('stream/gemma:tag'));
  for(const surface of ['chat_completions','messages','responses']) {
    await guide.locator('select').filter({has:page.locator('option[value="chat_completions"]')}).selectOption(surface);
    const endpoint=surface==='chat_completions'?'chat/completions':surface;
    await page.waitForFunction(endpoint=>document.querySelector('.kc-api-example')?.textContent.includes('/nt/v1/'+endpoint),endpoint);
    const code=await guide.locator('.kc-api-example').innerText();
    assert.ok(code.includes('"stream": true') && code.includes('stream/gemma:tag'));
    if(surface==='messages')assert.ok(code.includes('anthropic-version')&&code.includes('x-api-key'));
    if(surface==='responses')assert.ok(code.includes('"store": false'));
    await guide.getByRole('button',{name:'python',exact:true}).click();
    assert.match(await guide.locator('.kc-api-example').innerText(),/model_dump_json/);
    await guide.getByRole('button',{name:'curl',exact:true}).click();
  }
  assert.ok(requests.some(path=>path.startsWith('/nt/v1/models?')&&path.includes('stream=true')));
  check('native_stream_selection_filters_capabilities_and_preserves_protocol_event_examples');
  await guide.locator('input[type="checkbox"]').uncheck();
  await page.waitForFunction(()=>document.querySelector('.kc-api-example')?.textContent.includes('gemma3:270m'));
  check('nonstream_and_stream_discovery_do_not_share_stale_results');
  const lifecycle = guide.locator('select').filter({has:page.locator('option[value="stateless"]')});
  assert.equal(await lifecycle.locator('option').count(),4);
  for(const workflow of ['stored','conversation','background']) {
    await lifecycle.selectOption(workflow);
    await page.waitForFunction(()=>document.querySelector('.kc-api-example')?.textContent.includes('"store": true'));
    const code=await guide.locator('.kc-api-example').innerText();
    assert.ok(code.includes('/nt/v1'));
    if(workflow==='conversation')assert.ok(code.includes('/conversations')&&!code.includes('previous_response_id'));
    if(workflow==='stored')assert.ok(code.includes('previous_response_id'));
    if(workflow==='background')assert.ok(code.includes('"background": true')&&code.includes('/cancel'));
  }
  assert.ok(requests.some(p=>p.startsWith('/nt/v1/models?')&&p.includes('managed=true')&&!p.includes('stream=true')));
  await guide.locator('input[type="checkbox"]').check();
  await page.waitForFunction(()=>document.querySelector('.kc-api-example')?.textContent.includes('starting_after'));
  assert.ok((await guide.locator('.kc-api-example').innerText()).includes('/nt/v1'));
  check('managed_response_modes_keep_state_in_the_selected_family_and_filter_cancellation_capability');
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
