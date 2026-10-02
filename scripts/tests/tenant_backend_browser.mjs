// Explicit real-backend browser smoke test. It never supplies credentials by
// default: every value is required through the task-owned test environment.
import assert from 'node:assert/strict';

const required = [
  'KC_BACKEND_BROWSER_APP_URL',
  'KC_BACKEND_BROWSER_TENANT_ID',
  'KC_BACKEND_BROWSER_FOREIGN_TENANT_ID',
  'KC_BACKEND_BROWSER_ADMIN_TOKEN',
  'KC_BACKEND_BROWSER_FOREIGN_TOKEN',
];
const missing = required.filter((name) => !process.env[name]);
if (missing.length) {
  console.error(`missing explicit backend browser variables: ${missing.join(', ')}`);
  process.exit(2);
}

const { chromium } = await import(process.env.KC_PLAYWRIGHT_MODULE || 'playwright');
const appUrl = new URL(process.env.KC_BACKEND_BROWSER_APP_URL);
const tenantId = process.env.KC_BACKEND_BROWSER_TENANT_ID;
const foreignTenantId = process.env.KC_BACKEND_BROWSER_FOREIGN_TENANT_ID;
const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const context = await browser.newContext();
const pageErrors = [];
const page = await context.newPage();
page.on('pageerror', (error) => pageErrors.push(error.message));

async function request(token, path) {
  return page.evaluate(async ({ token: authorization, path: requestPath }) => {
    const response = await fetch(requestPath, {
      headers: { Authorization: `Bearer ${authorization}`, Accept: 'application/json' },
    });
    let body = null;
    try { body = await response.json(); } catch {}
    return { status: response.status, body };
  }, { token, path });
}

try {
  await page.goto(appUrl, { waitUntil: 'domcontentloaded' });
  const endpoint = `/api/v1/tenants/${encodeURIComponent(tenantId)}`;
  const foreignEndpoint = `/api/v1/tenants/${encodeURIComponent(foreignTenantId)}`;
  const own = await request(process.env.KC_BACKEND_BROWSER_ADMIN_TOKEN, endpoint);
  assert.ok(own.status >= 200 && own.status < 300, `admin tenant read returned ${own.status}`);
  assert.equal(own.body?.id, tenantId, 'admin response returned a different tenant');

  const adminCrossTenant = await request(process.env.KC_BACKEND_BROWSER_ADMIN_TOKEN, foreignEndpoint);
  assert.ok([401, 403, 404].includes(adminCrossTenant.status),
    `admin cross-tenant read unexpectedly returned ${adminCrossTenant.status}`);

  const foreignOwn = await request(process.env.KC_BACKEND_BROWSER_FOREIGN_TOKEN, foreignEndpoint);
  assert.ok(foreignOwn.status >= 200 && foreignOwn.status < 300,
    `foreign admin tenant read returned ${foreignOwn.status}`);
  assert.equal(foreignOwn.body?.id, foreignTenantId, 'foreign response returned a different tenant');

  const foreignCrossTenant = await request(process.env.KC_BACKEND_BROWSER_FOREIGN_TOKEN, endpoint);
  assert.ok([401, 403, 404].includes(foreignCrossTenant.status),
    `foreign cross-tenant read unexpectedly returned ${foreignCrossTenant.status}`);
  assert.deepEqual(pageErrors, [], 'browser page emitted an unhandled error');
  console.log(JSON.stringify({
    passed: true,
    tenant_id: tenantId,
    foreign_tenant_id: foreignTenantId,
    own_status: own.status,
    admin_cross_tenant_status: adminCrossTenant.status,
    foreign_own_status: foreignOwn.status,
    foreign_cross_tenant_status: foreignCrossTenant.status,
  }));
} finally {
  await browser.close();
}
