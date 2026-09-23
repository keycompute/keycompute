// Actual WASM, fresh browser contexts and synthetic HTTP only. No live worker or payment.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.KC_PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(process.env.KC_WEB_ROOT||'target/dx/web/debug/web/public'),out=path.resolve(process.env.KC_BROWSER_OUTPUT||'tenant-nodes-browser-results');
await fs.access(path.join(root,'index.html'));await fs.mkdir(out,{recursive:true});
const A='11111111-1111-4111-8111-111111111111',B='22222222-2222-4222-8222-222222222222',U='33333333-3333-4333-8333-333333333333',P='44444444-4444-4444-8444-444444444444';
const N='55555555-5555-4555-8555-555555555555',R='66666666-6666-4666-8666-666666666666',Q='77777777-7777-4777-8777-777777777777',L='88888888-8888-4888-8888-888888888888',F='99999999-9999-4999-8999-999999999999';
const stamp='2026-09-23T00:00:00.000001Z',prefix=`/api/v1/tenants/${A}`,errors=[],cases=[];
const server=http.createServer(async(req,res)=>{try{const selected=path.resolve(root,'.'+decodeURIComponent(new URL(req.url,'http://localhost').pathname));if(selected!==root&&!selected.startsWith(root+path.sep))throw Error('outside bundle');let file=selected,data;try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}const types={'.html':'text/html','.css':'text/css','.js':'text/javascript','.wasm':'application/wasm','.svg':'image/svg+xml','.png':'image/png'};res.writeHead(200,{'Content-Type':types[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);}catch{res.writeHead(500);res.end();}});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const base=`http://127.0.0.1:${server.address().port}`;let browser,latest;
async function fixture(initial='node-admin'){
 const context=await browser.newContext({viewport:{width:1440,height:1050}});await context.addInitScript(initial=>{localStorage.setItem('keyc_lang','en');if(!sessionStorage.getItem('initialized')){localStorage.setItem('keyc_access_token',initial);sessionStorage.setItem('initialized','yes');}},initial);
 const page=await context.newPage();page.setDefaultTimeout(20000);page.on('pageerror',e=>errors.push(e.message));
 const node={id:N,tenant_id:A,owner_user_id:P,display_name:'Fixture Node',status:'online',consecutive_failure_count:2,failure_threshold:3,last_heartbeat_at:stamp,created_at:stamp,updated_at:stamp};
 const registration={id:R,tenant_id:A,user_id:P,token_preview:'registration-preview-only',status:'pending',is_revealed:false,approved_by:null,actioned_at:null,consumed_at:null,consumed_node_id:null,issued_at:stamp,updated_at:stamp};
 function task(id,status){return {id,request_id:id,tenant_id:A,user_id:P,model:`model-${status}`,status,assigned_node_id:status==='leased'?N:null,failure_count:0,failure_threshold:3,queued_at:stamp,claimed_at:null,finished_at:status==='failed'?stamp:null,deadline_at:'2099-01-01T00:00:00Z',created_at:stamp,updated_at:stamp,cancellation_requested_at:null,archived_at:null};}
 const state={calls:[],unknown:[],node,registration,tasks:[task(Q,'queued'),task(L,'leased'),task(F,'failed')],version:1,evidence:true,unavailable:false,hold:null,seen:null};
 const next=()=>`2026-09-23T00:00:00.${String(++state.version).padStart(6,'0')}Z`;
 function profile(token){const tenant=token==='node-admin'?A:token==='node-member'?B:null;return {id:U,email:'admin@fixture.invalid',name:'Fixture tenant admin',platform_role:token==='node-root'?'root':token==='node-operator'?'operator':'none',status:'active',created_at:stamp,selected_tenant:tenant?{id:tenant,name:tenant===A?'Tenant A':'Tenant B',slug:tenant===A?'a':'b',tenant_role:tenant===A?'admin':'member',authz_version:1,membership_authz_version:1}:null,memberships:[{tenant_id:A,tenant_name:'Tenant A',tenant_role:'admin',status:'active',authz_version:1},{tenant_id:B,tenant_name:'Tenant B',tenant_role:'member',status:'active',authz_version:1}],capabilities:{platform:token==='node-root'?['users:manage']:token==='node-operator'?['platform:node_operations']:[],tenant:tenant===A?['tenant:manage']:[]}};}
 function session(token){return {...profile(token),user_id:U,access_token:token,token_type:'Bearer',expires_in:3600};}
 function paged(rows,url){const n=Number(url.searchParams.get('page')||1),s=Number(url.searchParams.get('page_size')||20);assert.equal(s,20);const owner=url.searchParams.get('owner_user_id'),status=url.searchParams.get('status'),search=url.searchParams.get('search');if(owner)rows=rows.filter(r=>(r.user_id||r.owner_user_id)===owner);if(status)rows=rows.filter(r=>r.status===status);if(search)rows=rows.filter(r=>(r.display_name||r.model||r.token_preview).includes(search));return {items:rows.slice((n-1)*s,n*s),total:rows.length,page:n,page_size:s,total_pages:Math.ceil(rows.length/s)};}
 await page.route('**/*',async route=>{
  const req=route.request(),url=new URL(req.url()),p=url.pathname,method=req.method();if(url.origin!==base)return route.abort();if(!p.startsWith('/api/'))return route.continue();
  const token=(req.headers().authorization||'').replace('Bearer ',''),body=req.postDataJSON();state.calls.push({method,path:p,query:url.search,body});let value,status=200,held=false;
  if(p==='/api/v1/settings/public')value={site_name:'KeyCompute',distribution_enabled:false};
  else if(p==='/api/v1/me')value=profile(token);
  else if(p==='/api/v1/me/tenant')value=session(body.tenant_id===A?'node-admin':'node-member');
  else if(p===`/api/v1/tenants/${A}`||p===`/api/v1/tenants/${B}`)value={id:p.endsWith(A)?A:B,owner_user_id:U,name:p.endsWith(A)?'Tenant A':'Tenant B',slug:p.endsWith(A)?'a':'b',description:null,status:'active',default_rpm_limit:60,default_tpm_limit:100000,authz_version:1,tenant_role:p.endsWith(A)?'admin':'member',membership_authz_version:1};
  else if(p===prefix+'/nodes')value=paged(state.node?[state.node]:[],url);
  else if(p===prefix+'/tasks')value=paged(state.tasks.filter(r=>Boolean(r.archived_at)===(url.searchParams.get('archived')==='true')),url);
  else if(p===prefix+'/node-registrations')value=paged([state.registration],url);
  else if(p.startsWith(prefix+'/nodes/'+N)){
    assert.ok(state.node);assert.equal(body?.expected_updated_at||url.searchParams.get('expected_updated_at'),state.node.updated_at);assert.ok((body?.reason||url.searchParams.get('reason')).trim());
    if(state.unavailable){status=503;value={error:{message:'synthetic uncertain node operation'}};}
    else if(method==='DELETE'&&state.evidence){status=409;value={error:{message:'Node retains required task evidence'}};}
    else {const deleted=method==='DELETE';if(method==='PATCH'){state.node.display_name=body.display_name;state.node.failure_threshold=body.failure_threshold;}else if(p.endsWith('/recover'))state.node.status='offline';else state.node.status='excluded';state.node.updated_at=next();value={node:{...state.node},changed:true,deleted};if(deleted)state.node=null;}
  }else if(p===prefix+'/node-registrations/'+R){
    assert.equal(method,'POST');assert.equal(body.expected_updated_at,state.registration.updated_at);assert.ok(body.reason);assert.ok(['approve','reject','revoke'].includes(body.action));state.registration.status=body.action==='approve'?'approved':'rejected';state.registration.updated_at=next();value={token:{...state.registration},changed:true,notification:body.action==='approve'?'failed':'not_applicable'};
  }else if(p.startsWith(prefix+'/tasks/')){
    const id=p.split('/').at(-2),action=p.split('/').at(-1),row=state.tasks.find(r=>r.id===id);assert.ok(row);assert.equal(method,'POST');assert.equal(body.expected_updated_at,row.updated_at);assert.ok(body.reason);
    if(id===L&&action==='cancel'){status=409;value={error:{message:'This leased task has no cancellation-aware worker; it must complete or expire.'}};}
    else if(state.hold&&id===Q){held=true;state.seen?.();await state.hold;row.cancellation_requested_at=next();row.status='failed';row.updated_at=next();value={task:{...row},changed:true,cancellation_requested:true,archived:false};}
    else {if(action==='archive')row.archived_at=next();else {row.cancellation_requested_at=next();row.status='failed';}row.updated_at=next();value={task:{...row},changed:true,cancellation_requested:Boolean(row.cancellation_requested_at),archived:Boolean(row.archived_at)};}
  }else{state.unknown.push(p);status=404;value={error:{message:'unknown tenant node fixture path'}};}
  await route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(value)}).catch(e=>{if(!held)throw e;});
 });
 latest={page,context,state};return latest;
}
const row=(page,id)=>page.locator('.tenant-node-table tbody tr').filter({hasText:id});
async function command(page,id,label,edit){await row(page,id).getByRole('button',{name:label,exact:true}).click();const dialog=page.getByRole('dialog');await dialog.waitFor();assert.equal(await dialog.getByRole('button',{name:'Confirm',exact:true}).isDisabled(),true);await dialog.locator('#node-command-reason').fill('Synthetic tenant operation');if(edit)await edit(dialog);await dialog.getByRole('button',{name:'Confirm',exact:true}).click();}
const writes=state=>state.calls.filter(r=>r.method!=='GET'&&r.path.startsWith(prefix));
try{
 browser=await chromium.launch({headless:true,args:['--no-sandbox']});const f=await fixture();const {page,state}=f;
 await page.goto(base+'/tenant/nodes');await row(page,N).waitFor();assert.equal(state.calls.filter(c=>c.path===prefix+'/tasks'||c.path===prefix+'/node-registrations').length,0);
 await command(page,N,'Configure node',async d=>{await d.locator('#node-name').fill('Configured Node');await d.locator('#node-threshold').fill('7');});await page.getByRole('dialog').waitFor({state:'detached'});await row(page,N).getByText('Configured Node',{exact:true}).waitFor();assert.equal(state.node.owner_user_id,P);assert.equal(state.node.failure_threshold,7);
 await command(page,N,'Exclude node');await page.getByRole('dialog').waitFor({state:'detached'});await row(page,N).getByText('excluded',{exact:true}).waitFor();
 await command(page,N,'Recover node');await page.getByRole('dialog').waitFor({state:'detached'});await row(page,N).getByText('offline',{exact:true}).waitFor();
 await command(page,N,'Revoke node admission');await page.getByRole('dialog').waitFor({state:'detached'});
 const before=writes(state).length;await command(page,N,'Delete node');await page.getByRole('dialog').getByRole('alert').waitFor();assert.equal(writes(state).length,before+1);await page.getByRole('dialog').getByRole('button',{name:'Cancel',exact:true}).click();assert.ok(state.node);
 state.evidence=false;await command(page,N,'Delete node');await page.getByRole('dialog').waitFor({state:'detached'});await page.getByText('No records',{exact:true}).waitFor();
 await page.getByRole('button',{name:'Registrations',exact:true}).click();await row(page,R).waitFor();await command(page,R,'Approve registration');await page.getByRole('dialog').waitFor({state:'detached'});await row(page,R).getByText('approved',{exact:true}).waitFor();await page.getByText('Registration result / notification status: approved; failed',{exact:true}).waitFor();assert.equal(state.registration.is_revealed,false);assert.equal(state.calls.filter(c=>/reveal|\/claim/.test(c.path)).length,0);
 await command(page,R,'Revoke registration');await page.getByRole('dialog').waitFor({state:'detached'});await row(page,R).getByText('rejected',{exact:true}).waitFor();assert.equal(await row(page,R).getByRole('button',{name:'Approve registration',exact:true}).count(),0);
 await page.getByRole('button',{name:'Tasks',exact:true}).click();await row(page,Q).waitFor();
 const attempts=writes(state).length;await command(page,L,'Request task cancellation');await page.getByRole('dialog').getByRole('alert').waitFor();assert.equal(writes(state).length,attempts+1);assert.equal(state.tasks.find(r=>r.id===L).status,'leased');await page.getByRole('dialog').getByRole('button',{name:'Cancel',exact:true}).click();
 await command(page,Q,'Request task cancellation');await page.getByRole('dialog').waitFor({state:'detached'});await row(page,Q).getByText('Cancellation requested',{exact:true}).waitFor();assert.equal(await row(page,Q).getByRole('button',{name:'Request task cancellation',exact:true}).count(),0);
 await command(page,F,'Archive task');await page.getByRole('dialog').waitFor({state:'detached'});await row(page,F).waitFor({state:'detached'});await page.locator('#node-archived').check();await page.getByRole('button',{name:'Apply query',exact:true}).click();await row(page,F).waitFor();assert.equal(await row(page,F).getByRole('button',{name:'Archive task',exact:true}).count(),0);
 assert.equal(state.tasks.find(r=>r.id===F).user_id,P);assert.equal(state.tasks.find(r=>r.id===F).request_id,F);
 await page.screenshot({path:path.join(out,'tasks.png'),fullPage:true});assert.deepEqual(state.unknown,[]);await f.context.close();cases.push('node configure/exclude/recover/revoke/delete evidence refusal; registration notification separate from approval; task cancellation refusal and archival preserve identity');
 const g=await fixture();await g.page.goto(base+'/tenant/nodes');await row(g.page,N).waitFor();g.state.unavailable=true;const attempts2=writes(g.state).length;await command(g.page,N,'Exclude node');await g.page.getByRole('dialog').getByRole('alert').waitFor();assert.equal(writes(g.state).length,attempts2+1);await g.page.getByRole('dialog').getByRole('button',{name:'Cancel',exact:true}).click();
 await g.page.locator('#node-owner').fill('not-a-uuid');const beforeSearch=g.state.calls.length;await g.page.getByRole('button',{name:'Apply query',exact:true}).click();await g.page.getByText('Check the owner UUID, search text and query bounds.',{exact:true}).waitFor();assert.equal(g.state.calls.length,beforeSearch);
 await g.page.locator('#node-owner').fill(P);const literal='%_&tenant_id='+B;await g.page.locator('#node-search').fill(literal);await g.page.getByRole('button',{name:'Apply query',exact:true}).click();await g.page.getByText('No records',{exact:true}).waitFor();const query=new URLSearchParams(g.state.calls.at(-1).query);assert.equal(query.get('search'),literal);assert.equal(query.get('owner_user_id'),P);assert.equal(query.has('tenant_id'),false);
 await g.page.locator('nav a[href="/tenant"]').click();await g.page.locator('#workspace-select').waitFor();await g.page.locator('#workspace-select').selectOption(B);await g.page.getByRole('button',{name:'Switch workspace',exact:true}).click();await g.page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant B');await g.page.goto(base+'/tenant/nodes');await g.page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(g.state.calls.filter(c=>c.path===`/api/v1/tenants/${B}/nodes`).length,0);assert.deepEqual(g.state.unknown,[]);await g.context.close();cases.push('uncertain writes single-dispatch; owner/literal filtering; member workspace never adopts tenant admin resource access');

 const delayed=await fixture();await delayed.page.goto(base+'/tenant/nodes');await row(delayed.page,N).waitFor();await delayed.page.getByRole('button',{name:'Tasks',exact:true}).click();await row(delayed.page,Q).waitFor();
 let releaseOld,markSeen;delayed.state.hold=new Promise(resolve=>releaseOld=resolve);const accepted=new Promise(resolve=>markSeen=resolve);delayed.state.seen=markSeen;
 await command(delayed.page,Q,'Request task cancellation');await accepted;
 // Simulate history navigation while the original component owns an in-flight command.
 await delayed.page.evaluate(()=>{history.pushState({},'', '/tenant');window.dispatchEvent(new PopStateEvent('popstate'));});
 await delayed.page.locator('#workspace-select').waitFor();await delayed.page.locator('#workspace-select').selectOption(B);await delayed.page.getByRole('button',{name:'Switch workspace',exact:true}).click();
 await delayed.page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant B');releaseOld();await delayed.page.waitForTimeout(200);
 assert.doesNotMatch(await delayed.page.locator('body').innerText(),/Cancellation request accepted|model-queued/);
 assert.equal(writes(delayed.state).filter(c=>c.path.endsWith('/cancel')).length,1);assert.equal(delayed.state.calls.filter(c=>c.path===`/api/v1/tenants/${B}/tasks`).length,0);assert.deepEqual(delayed.state.unknown,[]);
 await delayed.context.close();cases.push('an in-flight A task command cannot publish results or replay after switching to member workspace B');
 for(const token of ['node-root','node-operator','node-member']){const h=await fixture(token);await h.page.goto(base+'/tenant/nodes');await h.page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(h.state.calls.filter(c=>c.path.startsWith(prefix)).length,0);assert.deepEqual(h.state.unknown,[]);await h.context.close();}cases.push('global root/operator and ordinary member do not synthesize a tenant node administration grant');
 assert.deepEqual(errors,[]);const report={passed:true,basis:'real compiled WASM; synthetic HTTP and metadata only',cases,page_errors:errors};await fs.writeFile(path.join(out,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(e){if(latest&&!latest.page.isClosed()){await latest.page.screenshot({path:path.join(out,'failure.png'),fullPage:true}).catch(()=>{});console.error(JSON.stringify({errors,last_calls:latest.state.calls.slice(-12),unknown:latest.state.unknown},null,2));}console.error(e);process.exitCode=1;
}finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
