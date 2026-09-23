// Actual compiled WASM; all HTTP is intercepted with synthetic same-origin data.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.KC_PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(process.env.KC_WEB_ROOT||'target/dx/web/release/web/public');
const output=path.resolve(process.env.KC_BROWSER_OUTPUT||'platform-pricing-browser-results');
await fs.mkdir(output,{recursive:true});
const A='11111111-1111-4111-8111-111111111111',B='22222222-2222-4222-8222-222222222222',U='33333333-3333-4333-8333-333333333333',G='44444444-4444-4444-8444-444444444444',T='55555555-5555-4555-8555-555555555555',N='66666666-6666-4666-8666-666666666666';
const stamp='2026-09-23T00:00:00Z';
const server=http.createServer(async(req,res)=>{try{
 const pathname=decodeURIComponent(new URL(req.url,'http://localhost').pathname);let file=path.resolve(root,'.'+pathname);
 if(file!==root&&!file.startsWith(root+path.sep))throw Error('invalid asset path');
 let data;try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}
 const types={'.html':'text/html','.js':'text/javascript','.wasm':'application/wasm','.css':'text/css','.woff2':'font/woff2','.png':'image/png','.jpg':'image/jpeg','.ico':'image/x-icon','.svg':'image/svg+xml'};
 res.writeHead(200,{'Content-Type':types[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
}catch{res.writeHead(500);res.end();}});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const base=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});const errors=[],cases=[];let current;
const price=(tenant,id,name)=>({id,scope_type:tenant?'tenant':'platform',tenant_id:tenant,model_name:name,billing_dimension:'provideraccount',currency:'CNY',input_price_per_1k:'1E-10',output_price_per_1k:'9999999999.9999999999',is_default:false,is_effective:true,effective_from:stamp,effective_until:null,created_at:stamp,version:7});
async function fixture({role='root',selected=A}={}){
 const state={role,selected,rows:[price(null,G,'global-price'),price(B,T,'tenant-B-price')],calls:[],unknown:[],hold:false,release:null,unavailable:false,foreign:false};
 const context=await browser.newContext({viewport:{width:1440,height:1000}});
 await context.addInitScript(()=>{localStorage.setItem('keyc_access_token','synthetic-root');localStorage.setItem('keyc_lang','en');});
 const page=await context.newPage();current=page;page.setDefaultTimeout(20000);page.on('pageerror',e=>errors.push(e.message));
 await page.route('**/*',async route=>{
  const req=route.request(),url=new URL(req.url());if(url.origin!==base)return route.abort();if(!url.pathname.startsWith('/api/'))return route.continue();
  const p=url.pathname,m=req.method(),body=req.postDataJSON();state.calls.push({path:p,method:m,query:url.search,body});
  const answer=(v,status=200)=>route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(v)});
  if(p==='/api/v1/settings/public')return answer({site_name:'KeyCompute',distribution_enabled:false});
  if(p==='/api/v1/me')return answer({id:U,email:'root@fixture.invalid',name:'Pricing reviewer',platform_role:role,status:'active',created_at:stamp,selected_tenant:selected?{id:selected,name:'Tenant A',tenant_role:'admin',authz_version:1,membership_authz_version:1}:null,memberships:selected?[{tenant_id:selected,tenant_name:'Tenant A',tenant_role:'admin',status:'active',authz_version:1}]:[],capabilities:{platform:role==='root'?['users:manage']:role==='operator'?['platform:tenant_health']:[],tenant:selected?['tenant:manage']:[]}});
  if(p==='/api/v1/dashboard/overview')return answer({as_of:stamp,stats:{total_requests:0,total_tokens:0,total_input_tokens:0,total_output_tokens:0,total_cost:'0'},active_key_count:0,active_keys:[],recent_usage:[],recent_orders:[],trend:{from:stamp,to:stamp,granularity:'day',as_of:stamp,buckets:[]}});
  if(p==='/api/v1/payments/balance')return answer({user_id:U,available_balance:'0',frozen_balance:'0',total_balance:'0',total_recharged:'0',total_consumed:'0',initialized:false,as_of:stamp});
  const prefix='/api/v1/platform/pricing';
  if(p===prefix&&m==='POST'){
   assert.ok(['platform','tenant'].includes(body.scope_type));if(body.scope_type==='platform')assert.equal(body.tenant_id,undefined);else assert.equal(body.tenant_id,B);
   if(state.unavailable)return answer({error:{message:'uncertain creation; refresh first'}},503);
   const r={...price(body.tenant_id||null,N,body.model_name),...body,effective_from:body.effective_from||stamp,version:1};state.rows.push(r);
   return answer({success:true,message:'created',pricing_id:r.id,model_name:r.model_name,billing_dimension:r.billing_dimension,input_price_per_1k:r.input_price_per_1k,output_price_per_1k:r.output_price_per_1k,is_default:r.is_default,version:r.version});
  }
  if(p.startsWith(prefix)){
   const target=url.searchParams.get('scope_type');assert.ok(['platform','tenant'].includes(target));const tenant=url.searchParams.get('tenant_id');assert.equal(tenant,target==='platform'?null:B);
   if(p===prefix&&m==='GET'){
    const n=Number(url.searchParams.get('page')),size=Number(url.searchParams.get('page_size'));let rows=state.rows.filter(r=>r.scope_type===target&&r.tenant_id===tenant);
    const search=url.searchParams.get('search');if(search)rows=rows.filter(r=>r.model_name.includes(search));const total=rows.length;
    return answer({pricing:state.foreign?[price(A,T,'MUST-NOT-RENDER')]:rows.slice((n-1)*size,n*size),total,page:n,page_size:size,total_pages:Math.ceil(total/size)});
   }
   const id=p.slice(prefix.length+1).split('/')[0];const r=state.rows.find(r=>r.id===id);assert.ok(r);assert.equal(r.tenant_id,tenant);
   if(m==='PUT'){
    if(state.unavailable)return answer({error:{message:'uncertain update; refresh first'}},503);
    if(state.hold)await new Promise(resolve=>state.release=resolve);
    assert.equal(body.expected_version,r.version);Object.assign(r,body,{version:r.version+1});return answer({success:true,message:'updated',pricing_id:r.id,version:r.version});
   }
   if(m==='POST'&&p.endsWith('/make-default')){r.is_default=true;r.version++;return answer({success:true,message:'default',pricing_id:r.id,version:r.version});}
   if(m==='DELETE'){assert.equal(target,'tenant');state.rows=state.rows.filter(v=>v.id!==id);return answer({success:true,message:'deleted',pricing_id:id});}
  }
  state.unknown.push(`${m} ${p}`);return answer({error:{message:'unhandled fixture route'}},404);
 });
 return {state,page,context};
}
async function target(page,mode,tenant=''){
 await page.locator('#platform-pricing-scope').selectOption(mode);if(mode==='tenant')await page.locator('#platform-pricing-tenant').fill(tenant);
 await page.getByRole('button',{name:'Apply target',exact:true}).click();
}
try{
 let {state,page,context}=await fixture();await page.goto(base+'/admin/pricing');await page.getByText('global-price',{exact:true}).waitFor();
 assert.equal(state.calls.find(c=>c.path==='/api/v1/platform/pricing').query.includes('scope_type=platform'),true);
 assert.equal(await page.locator('tbody').getByRole('button',{name:'Delete',exact:true}).count(),0);
 assert.match(await page.locator('tbody').innerText(),/9999999999\.9999999999/);
 await page.locator('tbody').getByRole('button',{name:'Edit',exact:true}).click();let d=page.getByRole('dialog');await d.waitFor();assert.equal(await d.locator('#platform-price-input').inputValue(),'0.0000000001');
 await d.locator('#platform-price-input').fill('0.0000000002');await d.getByRole('button',{name:'Save Changes',exact:true}).click();await d.waitFor({state:'detached'});
 assert.equal(state.calls.find(c=>c.method==='PUT').body.expected_version,7);
 await page.locator('tbody').getByRole('button',{name:'Set Default',exact:true}).click();await page.waitForFunction(()=>!document.querySelector('tbody')?.textContent.includes('Set Default'));
 await page.getByRole('button',{name:'+ Create Pricing',exact:true}).click();d=page.getByRole('dialog');await d.locator('#platform-price-model').fill('global-added');await d.locator('#platform-price-input').fill('0.1');await d.locator('#platform-price-output').fill('0.2');await d.getByRole('button',{name:'Create',exact:true}).click();await d.waitFor({state:'detached'});await page.getByText('global-added',{exact:true}).waitFor();
 await target(page,'tenant',B);await page.getByText('tenant-B-price',{exact:true}).waitFor();assert.doesNotMatch(await page.locator('tbody').innerText(),/global-price|global-added/);
 await page.locator('tbody').getByRole('button',{name:'Delete',exact:true}).click();d=page.getByRole('alertdialog');await d.getByRole('button',{name:'Delete',exact:true}).click();await page.waitForFunction(()=>!document.querySelector('tbody')?.textContent.includes('tenant-B-price'));
 const before=state.calls.length;await target(page,'tenant','not-a-tenant');await page.locator('.platform-pricing-workspace [role="alert"]').first().waitFor();assert.equal(state.calls.length,before);
 await target(page,'tenant',B);state.unavailable=true;await page.getByRole('button',{name:'+ Create Pricing',exact:true}).click();d=page.getByRole('dialog');await d.locator('#platform-price-model').fill('uncertain-tenant');await d.locator('#platform-price-input').fill('1');await d.locator('#platform-price-output').fill('1');await d.getByRole('button',{name:'Create',exact:true}).click();await d.locator('.alert-error').waitFor();
 assert.equal(state.calls.filter(c=>c.method==='POST'&&c.path==='/api/v1/platform/pricing').length,2);await d.getByRole('button',{name:'Cancel',exact:true}).click();
 assert.equal(state.calls.filter(c=>c.path==='/api/v1/me/tenant').length,0);assert.deepEqual(state.unknown,[]);
 await page.screenshot({path:path.join(output,'explicit-target.png')});cases.push('explicit platform/tenant selection, exact prices and versions, creation/default/deletion and uncertain command single-dispatch');await context.close();
 ({state,page,context}=await fixture());state.hold=true;await page.goto(base+'/admin/pricing');await page.getByText('global-price',{exact:true}).waitFor();await page.locator('tbody').getByRole('button',{name:'Edit',exact:true}).click();d=page.getByRole('dialog');await d.locator('#platform-price-input').fill('0.7777777777');await d.getByRole('button',{name:'Save Changes',exact:true}).click();
 for(let i=0;i<100&&!state.release;i++)await new Promise(r=>setTimeout(r,10));assert.ok(state.release);
 await d.getByRole('button',{name:'Cancel',exact:true}).click();await target(page,'tenant',B);await page.getByText('tenant-B-price',{exact:true}).waitFor();state.release();await page.waitForTimeout(150);
 assert.doesNotMatch(await page.locator('.platform-pricing-records').innerText(),/0\.7777777777|Pricing updated successfully|global-price/);assert.equal(state.calls.filter(c=>c.method==='PUT').length,1);assert.deepEqual(state.unknown,[]);await context.close();cases.push('target change discards pending old-target forms and completion callbacks');
 ({state,page,context}=await fixture({selected:null}));state.foreign=true;await page.goto(base+'/admin/pricing');await page.locator('.platform-pricing-records [role="alert"]').waitFor();assert.equal(await page.getByText('MUST-NOT-RENDER',{exact:true}).count(),0);await context.close();
 for(const role of ['operator','none']){({state,page,context}=await fixture({role}));await page.goto(base+'/admin/pricing');await page.waitForURL('**/dashboard');assert.equal(state.calls.filter(c=>c.path.includes('/pricing')).length,0);assert.deepEqual(state.unknown,[]);await context.close();}
 cases.push('foreign result rejected and tenant/operator capabilities never mount platform pricing');
 assert.deepEqual(errors,[]);const report={passed:true,basis:'production-mode WASM with synthetic HTTP, not production or backend isolation evidence',cases,page_errors:errors};await fs.writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(e){if(current&&!current.isClosed()){console.error((await current.locator('body').innerText()).slice(0,6000));await current.screenshot({path:path.join(output,'failure.png')}).catch(()=>{});}console.error(e);process.exitCode=1;
}finally{await browser.close();await new Promise(r=>server.close(r));}
