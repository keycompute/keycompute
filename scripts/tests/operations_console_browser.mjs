// Actual compiled WASM. Every HTTP API response is an isolated synthetic fixture.
// This verifies presentation/race behavior, not backend authorization or production data.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.KC_PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(process.env.KC_WEB_ROOT||'target/dx/web/release/web/public');
const out=path.resolve(process.env.KC_BROWSER_OUTPUT||'operations-browser-results');
await fs.access(path.join(root,'index.html'));await fs.mkdir(out,{recursive:true});
const A='11111111-1111-4111-8111-111111111111', B='22222222-2222-4222-8222-222222222222', U='33333333-3333-4333-8333-333333333333';
const prefix='/api/v1/platform/operations', stamp='2026-09-23T00:00:00Z';
const capabilities=['platform:tenant_health','platform:aggregate_stats','platform:diagnostics'];
const errors=[],cases=[];
const server=http.createServer(async(req,res)=>{
  try{
    const selected=path.resolve(root,'.'+decodeURIComponent(new URL(req.url,'http://localhost').pathname));
    if(selected!==root&&!selected.startsWith(root+path.sep))throw Error('outside fixture bundle');
    let file=selected,data;try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}
    const types={'.wasm':'application/wasm','.js':'text/javascript','.html':'text/html','.css':'text/css','.svg':'image/svg+xml','.png':'image/png'};
    res.writeHead(200,{'Content-Type':types[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
  }catch{res.writeHead(500);res.end();}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const base=`http://127.0.0.1:${server.address().port}`;
let browser,latest;
async function fixture({role='operator',permissions=capabilities,tenantCapabilities=[]}={}){
  const context=await browser.newContext({viewport:{width:1440,height:1000}});
  await context.addInitScript(()=>{localStorage.setItem('keyc_lang','en');localStorage.setItem('keyc_access_token','synthetic-operator');});
  const page=await context.newPage();page.setDefaultTimeout(20000);page.on('pageerror',e=>errors.push(e.message));
  const state={calls:[],unknown:[],permissions:[...permissions],hold:null,seen:null,capacityError:false};
  const profile=()=>({id:U,email:'operator@fixture.invalid',name:'Fixture Operator',status:'active',platform_role:role,created_at:stamp,token_version:0,memberships:[],selected_tenant:null,capabilities:{platform:state.permissions,tenant:tenantCapabilities}});
  const row=i=>({tenant_id:i===0?A:i===20?B:`44444444-4444-4444-8444-${String(i).padStart(12,'0')}`,name:`Fixture Tenant ${i}`,slug:`fixture-${i}`,status:i===20?'inactive':'active',default_rpm_limit:60,default_tpm_limit:100000,active_members:3,active_admins:1,suspended_members:1,provider_accounts:2,enabled_accounts:1,online_nodes:1,excluded_nodes:0,queued_tasks:4,leased_tasks:2});
  await page.route('**/*',async route=>{
    const req=route.request(),u=new URL(req.url());if(u.origin!==base)return route.abort();if(!u.pathname.startsWith('/api/'))return route.continue();
    const p=u.pathname;state.calls.push({method:req.method(),path:p,query:u.search});
    let body,status=200,held=false;
    if(p==='/api/v1/settings/public')body={site_name:'KeyCompute',distribution_enabled:false};
    else if(p==='/api/v1/me')body=profile();
    else if(p===prefix+'/tenants'){
      assert.equal(req.method(),'GET');let rows=Array.from({length:21},(_,i)=>row(i));const search=u.searchParams.get('search'),filter=u.searchParams.get('status');
      if(search)rows=rows.filter(r=>r.name.includes(search)||r.slug.includes(search));if(filter)rows=rows.filter(r=>r.status===filter);
      const limit=Number(u.searchParams.get('limit')),offset=Number(u.searchParams.get('offset'));
      assert.equal(limit,20);body={items:rows.slice(offset,offset+limit),total:rows.length,limit,offset,as_of:stamp};
    }else if(p===prefix+'/tenants/'+A||p===prefix+'/tenants/'+B)body=row(p.endsWith(A)?0:20);
    else if(p===prefix+'/usage'||p===prefix+'/tenants/'+A+'/usage'||p===prefix+'/tenants/'+B+'/usage'){
      if(state.hold&&p.includes(A)){held=true;state.seen?.();await state.hold;}
      const amount=p.includes(A)?'91.1234567890':p.includes(B)?'12.0000000001':'103.1234567891';
      body={from:u.searchParams.get('from'),to:u.searchParams.get('to'),as_of:stamp,currencies:[{currency:'CNY',requests:3,successful_requests:2,total_tokens:'10000000000000000001',billed_amount:amount},{currency:'USD',requests:1,successful_requests:1,total_tokens:'5',billed_amount:'0.0000000001'}]};
    }else if(p===prefix+'/capacity'){
      if(state.capacityError){status=503;body={error:{message:'synthetic capacity unavailable'}};}
      else body={scope:'application_process',managed_payload_bytes:{limit:1000000,used:24,peak:32},ingress:{active:1,queued:0},generation:{active:1,queued:0},writer_pool:{connections:7,idle:null,url:'MUST-NOT-DISPLAY-SECRET'},redis_commands:null,redis_cache:{connections:'MUST-NOT-DISPLAY-SECRET'},unexpected:'MUST-NOT-DISPLAY-SECRET'};
    }else{state.unknown.push(p);status=404;body={error:{message:'unexpected operations fixture endpoint'}};}
    await route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(body)}).catch(e=>{if(!held)throw e;});
  });
  latest={page,context,state};return latest;
}
function apiCalls(state){return state.calls.filter(c=>c.path.startsWith(prefix));}
async function shot(page,name){await page.screenshot({path:path.join(out,`${name}.png`),fullPage:true});}
try{
  browser=await chromium.launch({headless:true,args:['--no-sandbox']});
  const {page,context,state}=await fixture();
  await page.goto(base+'/platform/operations');await page.locator('.operations-health tbody tr').filter({hasText:A}).waitFor();
  assert.equal(await page.locator('#app-sidebar').getByRole('button',{name:'Platform operations',exact:true}).count(),1);
  assert.equal(await page.locator('#app-sidebar').getByRole('button',{name:'Users',exact:true}).count(),0);
  assert.ok(apiCalls(state).every(c=>c.method==='GET'));assert.equal(apiCalls(state).length,1,'inactive panels must not prefetch');
  await page.locator('tbody tr').first().getByRole('button',{name:'View health',exact:true}).click();
  await page.locator('.operations-detail').getByText('RPM: 60 · TPM: 100000',{exact:true}).waitFor();
  await page.getByRole('button',{name:'Next',exact:true}).click();await page.locator('.operations-health tbody tr').filter({hasText:B}).waitFor();
  assert.equal(await page.locator('.operations-detail').count(),0);
  assert.equal(apiCalls(state).at(-1).query,'?limit=20&offset=20');
  const literal='%_&tenant_id='+B;
  await page.locator('#operations-search').fill(literal);await page.getByRole('button',{name:'Apply query',exact:true}).click();
  await page.getByText('No records',{exact:true}).waitFor();
  let query=new URLSearchParams(apiCalls(state).at(-1).query);assert.equal(query.get('search'),literal);assert.equal(query.get('offset'),'0');assert.equal(query.has('tenant_id'),false);
  await page.locator('#operations-search').fill('');await page.locator('#operations-status').selectOption('inactive');await page.getByRole('button',{name:'Apply query',exact:true}).click();
  await page.locator('.operations-health tbody tr').filter({hasText:B}).waitFor();await shot(page,'health');
  await page.getByRole('button',{name:'Usage aggregates',exact:true}).click();await page.getByText('103.1234567891',{exact:true}).waitFor();
  await page.getByText('10000000000000000001',{exact:true}).waitFor();await page.getByText('USD',{exact:true}).waitFor();
  await page.locator('#operations-target').selectOption('tenant');await page.locator('#operations-tenant-id').fill('invalid');
  const before=apiCalls(state).length;await page.getByRole('button',{name:'Apply query',exact:true}).click();
  await page.getByText('Provide a valid target and UTC range of no more than 31 days.',{exact:true}).waitFor();assert.equal(apiCalls(state).length,before);
  let release,seen;state.hold=new Promise(resolve=>release=resolve);const waiting=new Promise(resolve=>seen=resolve);state.seen=seen;
  await page.locator('#operations-tenant-id').fill(A);await page.getByRole('button',{name:'Apply query',exact:true}).click();await waiting;
  assert.equal(await page.getByText('103.1234567891',{exact:true}).count(),0,'pending target must not retain old aggregate');
  await page.locator('#operations-tenant-id').fill(B);await page.getByRole('button',{name:'Apply query',exact:true}).click();
  await page.getByText('12.0000000001',{exact:true}).waitFor();release();await page.waitForTimeout(150);
  assert.equal(await page.getByText('91.1234567890',{exact:true}).count(),0,'late A result cannot overwrite B');await shot(page,'usage');
  await page.getByRole('button',{name:'Process capacity',exact:true}).click();await page.getByText('writer_pool.connections',{exact:true}).waitFor();
  assert.doesNotMatch(await page.locator('.operations-capacity').innerText(),/MUST-NOT-DISPLAY/);
  assert.match(await page.locator('tr').filter({hasText:'writer_pool.idle'}).innerText(),/—/);
  state.capacityError=true;await page.getByRole('button',{name:'Refresh data',exact:true}).click();await page.locator('.operations-capacity [role="alert"]').waitFor();
  assert.equal(await page.getByText('writer_pool.connections',{exact:true}).count(),0,'failed refresh cannot show a retained snapshot');
  assert.deepEqual(state.unknown,[]);assert.ok(apiCalls(state).every(c=>c.method==='GET'));
  cases.push('global operator health/detail/literal filters/pagination; exact per-currency aggregates; explicit target and stale-result fencing; safe capacity/errors');
  await context.close();
  for(const options of [{role:'none',permissions:[],tenantCapabilities:capabilities},{role:'root',permissions:[]},{role:'operator',permissions:['platform:node_operations']}]){
    const f=await fixture(options);await f.page.goto(base+'/platform/operations');await f.page.getByText('This session has no platform operations capability.',{exact:true}).waitFor();
    assert.equal(apiCalls(f.state).length,0);assert.equal(await f.page.locator('#app-sidebar').getByRole('button',{name:'Platform operations',exact:true}).count(),0);assert.deepEqual(f.state.unknown,[]);await f.context.close();
  }
  cases.push('tenant permissions, role labels and unrelated operator capabilities do not enable operations');
  for(const [cap,expected,selector] of [['platform:tenant_health','/tenants','Fixture Tenant 0'],['platform:aggregate_stats','/usage','103.1234567891'],['platform:diagnostics','/capacity','writer_pool.connections']]){
    const f=await fixture({permissions:[cap]});await f.page.goto(base+'/platform/operations');if(expected==='/tenants'){await f.page.locator('.operations-health tbody tr').filter({hasText:A}).waitFor();}else{await f.page.getByText(selector,{exact:true}).waitFor();}
    assert.equal(apiCalls(f.state).length,1);assert.equal(apiCalls(f.state)[0].path,prefix+expected);assert.equal(await f.page.locator('.operations-console > nav button').count(),1);assert.deepEqual(f.state.unknown,[]);await f.context.close();
  }
  cases.push('partial capabilities fetch and display only their own allowlisted section');
  assert.deepEqual(errors,[]);const report={passed:true,basis:'real compiled WASM; synthetic intercepted HTTP only',cases,page_errors:errors};
  await fs.writeFile(path.join(out,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(e){
  if(latest&&!latest.page.isClosed())await latest.page.screenshot({path:path.join(out,'failure.png'),fullPage:true}).catch(()=>{});
  console.error(e);process.exitCode=1;
}finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
