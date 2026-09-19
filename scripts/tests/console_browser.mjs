// Actual WASM UI regression with controlled in-browser HTTP mocks, not a backend benchmark.
// KC_WEB_ROOT points at a built web/public folder; never attach a user's browser session.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.KC_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(process.env.KC_WEB_ROOT || 'target/dx/web/release/web/public');
const output = path.resolve(process.env.KC_BROWSER_OUTPUT || 'console-browser-results');
await fs.mkdir(output, {recursive:true});
const counts={}, errors=[], unknown=[]; let identity='A', limited=false;
const server=http.createServer(async(req,res)=>{
  try {
    const pathname=decodeURIComponent(new URL(req.url,'http://localhost').pathname);
    const file=path.resolve(root,'.'+pathname);
    if(file!==root&&!file.startsWith(root+path.sep))throw Error('invalid asset path');
    let data,selected=file;
    try { data=await fs.readFile(file); } catch { selected=path.join(root,'index.html'); data=await fs.readFile(selected); }
    const types={'.wasm':'application/wasm','.js':'text/javascript','.css':'text/css','.html':'text/html','.svg':'image/svg+xml','.png':'image/png'};
    res.writeHead(200,{'Content-Type':types[path.extname(selected)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
  } catch { res.writeHead(500);res.end(); }
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
const stamp='2026-09-19T04:00:00Z';
const stats={total_requests:250,total_tokens:750,total_input_tokens:250,total_output_tokens:500,total_prompt_tokens:250,total_completion_tokens:500,total_cost:12.34,period:'all_time'};
const trend={from:'2026-09-13T00:00:00Z',to:'2026-09-20T00:00:00Z',granularity:'day',as_of:stamp,
  buckets:[150,100,0].map((requests,i)=>({start:`2026-09-${13+i}T00:00:00Z`,requests,total_tokens:requests*3,total_cost:'0.000000015'}))};
try {
  const context=await browser.newContext({viewport:{width:1440,height:1000}});
  await context.addInitScript(()=>localStorage.setItem('keyc_access_token','mock-A'));
  const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
  const paged=(field,u)=>({[field]:[],total:0,page:Number(u.searchParams.get('page')||1),page_size:Number(u.searchParams.get('page_size')||20),total_pages:0});
  await page.route('**/*',async route=>{
    const request=route.request(),u=new URL(request.url());
    if(u.origin!==base)return route.abort();
    if(!u.pathname.startsWith('/api/'))return route.continue();
    const p=u.pathname;counts[p]=(counts[p]||0)+1;let body;
    const uid=identity==='A'?'11111111-1111-4111-8111-111111111111':'33333333-3333-4333-8333-333333333333';
    const tenant='22222222-2222-4222-8222-222222222222';
    const earnings={total_earnings:identity==='A'?'123.45':'987.65',settled_amount:'100',pending_amount:'23.45',currency:'CNY',level1_referrals:0,level2_referrals:0};
    if(p==='/api/v1/settings/public')body={site_name:'KeyCompute',distribution_enabled:true};
    else if(p==='/api/v1/auth/login') { identity='B'; body={user_id:'33333333-3333-4333-8333-333333333333',tenant_id:tenant,email:'mock-B@example.invalid',role:'user',access_token:'mock-B',token_type:'Bearer',expires_in:3600}; }
    else if(p==='/api/v1/me')body={id:uid,tenant_id:tenant,email:`mock-${identity}@example.invalid`,name:`Mock ${identity}`,role:'user',created_at:stamp};
    else if(p==='/api/v1/me/distribution/overview')body={as_of:stamp,earnings,referral:{referral_code:identity,invite_link:base+'/auth/register?ref='+identity}};
    else if(p==='/api/v1/me/distribution/earnings')body=earnings;
    else if(p==='/api/v1/me/distribution/referrals')body=paged('referrals',u);
    else if(p==='/api/v1/usage/trend')body=trend;
    else if(p==='/api/v1/dashboard/overview')body={as_of:stamp,stats:{...stats,total_cost:'12.34'},active_key_count:0,active_keys:[],recent_usage:[],recent_orders:[],trend};
    else if(p==='/api/v1/payments/balance'){
      if(limited)return route.fulfill({status:429,contentType:'application/json',headers:{'Retry-After':'30','X-RateLimit-Scope':'console_read'},body:JSON.stringify({error:{message:'Rate limit exceeded'}})});
      body={user_id:uid,available_balance:'100.25',frozen_balance:'0',total_balance:'100.25',total_recharged:'100.25',total_consumed:'0',initialized:true,as_of:stamp};
    } else if(p==='/api/v1/payments/methods')body=[];
    else if(p==='/api/v1/payments/orders')body=paged('orders',u);
    else if(p==='/api/v1/usage/stats')body=stats;
    else if(p==='/api/v1/usage')body=paged('records',u);
    else { unknown.push(p);return route.fulfill({status:404,contentType:'application/json',body:'{}'}); }
    await route.fulfill({status:200,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(body)});
  });
  const nav=async(name,suffix)=>{await page.locator('#app-sidebar').getByRole('button',{name,exact:true}).click();await page.waitForURL('**/'+suffix);};
  await page.goto(base+'/distribution');await page.locator('.distribution-invite-copy-section').waitFor();
  assert.match(await page.locator('.stat-value').first().innerText(),/123\.45/);
  const before={...counts},started=Date.now();
  for(let i=0;i<4;i++){
    await nav('用量统计','usage');
    await page.locator('#usage-line-chart').waitFor();
    await nav('分销中心','distribution');await page.locator('.distribution-invite-copy-section').waitFor();
  }
  const elapsed=Date.now()-started;
  assert.equal(counts['/api/v1/me/referral/code']||0,0,'overview must replace link-only requests');
  assert.equal(counts['/api/v1/me/distribution/overview'],before['/api/v1/me/distribution/overview'],'navigation inside TTL must reuse overview');
  assert.equal(counts['/api/v1/usage/trend'],1,'one complete trend, not repeated last-100 fetches');
  await page.screenshot({path:path.join(output,'distribution.png')});
  await nav('仪表盘','dashboard');await page.locator('.dashboard-stat-value').first().waitFor();
  await page.waitForFunction(()=>document.querySelector('.dashboard-stat-value')?.textContent==='250');
  assert.equal(counts['/api/v1/keys']||0,0,'dashboard must not fetch all keys');
  assert.equal(counts['/api/v1/payments/orders']||0,0,'dashboard must use bounded embedded order previews');
  await page.screenshot({path:path.join(output,'dashboard.png')});
  // Allow cached balance to expire, then verify one 429 cools down subsequent callers.
  await page.waitForTimeout(2100);limited=true;
  await nav('支付中心','payments');await page.locator('[role="alert"]').first().waitFor();
  assert.equal(await page.locator('.stat-value').first().innerText(),'—');
  const rejectedCount=counts['/api/v1/payments/balance'];
  await nav('用量统计','usage');await nav('支付中心','payments');
  await page.locator('[role="alert"]').first().waitFor();
  assert.equal(counts['/api/v1/payments/balance'],rejectedCount,'429 must not trigger duplicate short retries');
  await page.screenshot({path:path.join(output,'rate-limit.png')});
  limited=false;
  await page.getByRole('button',{name:'用户菜单',exact:true}).click();
  await page.getByRole('menuitem',{name:'退出登录',exact:true}).click();
  await page.waitForURL(base+'/');
  assert.equal(await page.locator('.distribution-invite-copy-section').count(),0);
  await page.getByRole('button',{name:'登录',exact:true}).first().click();
  const dialog=page.getByRole('dialog');await dialog.waitFor();
  await dialog.locator('input[type="email"]').fill('mock-B@example.invalid');
  await dialog.locator('input[type="password"]').fill('mock-password-only');
  await dialog.locator('button[type="submit"]').click();
  await page.waitForURL('**/dashboard');
  await nav('分销中心','distribution');await page.locator('.distribution-invite-copy-section').waitFor();
  assert.match(await page.locator('.stat-value').first().innerText(),/987\.65/);
  assert.doesNotMatch(await page.locator('.distribution-invite-copy-section').innerText(),/ref=A/);
  assert.match(await page.locator('.distribution-invite-copy-section').innerText(),/ref=B/);
  const report={scope:'actual production-pinned WASM with controlled HTTP mocks; not server throughput',before,after:counts,
    cycles:4,elapsed_ms:elapsed,page_errors:errors,unhandled_api_paths:unknown,checks:['overview dedup','complete trend endpoint','bounded dashboard fetches','429 cooldown and separate error display','logout/new-login identity isolation']};
  await fs.writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
  assert.deepEqual(errors,[]);assert.deepEqual(unknown,[]);
} catch(error) {
  console.error(error);process.exitCode=1;
} finally { await browser.close();await new Promise(resolve=>server.close(resolve)); }
