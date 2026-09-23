// Actual compiled WASM, isolated browser contexts and synthetic HTTP fixtures.
// Never reads application credentials or connects to a production backend.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.KC_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(process.env.KC_WEB_ROOT || 'target/dx/web/debug/web/public');
const out = path.resolve(process.env.KC_BROWSER_OUTPUT || 'tenant-browser-results');
await fs.access(path.join(root, 'index.html'));
await fs.mkdir(out, { recursive: true });
const A='11111111-1111-4111-8111-111111111111', B='22222222-2222-4222-8222-222222222222';
const U='33333333-3333-4333-8333-333333333333', P='44444444-4444-4444-8444-444444444444', Q='55555555-5555-4555-8555-555555555555';
const I='66666666-6666-4666-8666-666666666666', SECRET='a'.repeat(64), stamp='2026-09-23T11:00:00Z';
const pageErrors=[], cases=[];
const server=http.createServer(async(req,res)=>{
  try {
    const selected=path.resolve(root,'.'+decodeURIComponent(new URL(req.url,'http://localhost').pathname));
    if(selected!==root&&!selected.startsWith(root+path.sep))throw Error('outside bundle');
    let file=selected, data; try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}
    const type={'.wasm':'application/wasm','.js':'text/javascript','.html':'text/html','.css':'text/css','.svg':'image/svg+xml','.png':'image/png'};
    res.writeHead(200,{'Content-Type':type[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
  }catch{res.writeHead(500);res.end();}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
let browser, latest;
async function fixture(initial='mock-A') {
  const context=await browser.newContext({viewport:{width:1440,height:1000}});
  await context.addInitScript(({initial})=>{
    localStorage.setItem('keyc_lang','en');
    if(initial&&!sessionStorage.getItem('fixture_initialized')){
      localStorage.setItem('keyc_access_token',initial);sessionStorage.setItem('fixture_initialized','1');
    }
  },{initial});
  const page=await context.newPage(); page.setDefaultTimeout(20000);
  page.on('pageerror',error=>pageErrors.push(error.message));
  const state={calls:[],unknown:[],invitation:null,createUnavailable:false,accepted:false,
    configVersion:1,owner:U,holdMembers:null,seenMembers:null,
    members:[member(U,'owner@fixture.invalid','admin'),member(P,'member@fixture.invalid','member'),member(Q,'admin@fixture.invalid','admin')]};
  function member(id,email,role){return {user_id:id,email,name:null,user_status:'active',tenant_role:role,membership_status:'active',authz_version:1,invited_by:null,joined_at:stamp,removed_at:null};}
  function selected(token){return token.startsWith('mock-A')?A:token.startsWith('mock-B')?B:null;}
  function role(token){return token==='mock-root'?'root':token==='mock-op'?'operator':'none';}
  function memberships(){return (initial===null||initial==='mock-expired')&&!state.accepted?[]:[
    {tenant_id:A,tenant_name:'Tenant A',tenant_role:(initial===null||initial==='mock-expired')?'member':'admin',status:'active',authz_version:1},
    {tenant_id:B,tenant_name:'Tenant B',tenant_role:'member',status:'active',authz_version:1}];}
  function profile(token){const tid=selected(token), membership=memberships().find(m=>m.tenant_id===tid);return {
    id:U,email:'owner@fixture.invalid',name:'Fixture user',status:'active',platform_role:role(token),token_version:0,created_at:stamp,
    memberships:memberships(),selected_tenant:tid?{id:tid,name:tid===A?'Tenant A':'Tenant B',slug:tid===A?'a':'b',tenant_role:membership?.tenant_role||'member',authz_version:state.configVersion,membership_authz_version:1}:null,
    capabilities:{platform:role(token)==='root'?['users:manage']:role(token)==='operator'?['platform:diagnostics']:[],tenant:membership?.tenant_role==='admin'?['tenant:manage','members:manage','invitations:manage']:[]}
  };}
  function session(token){const p=profile(token);return {...p,user_id:U,access_token:token,token_type:'Bearer',expires_in:3600};}
  function tenant(tid){return {id:tid,owner_user_id:state.owner,name:tid===A?'Tenant A':'Tenant B',slug:tid===A?'a':'b',description:'Scoped fixture',status:'active',default_rpm_limit:60,default_tpm_limit:100000,authz_version:state.configVersion,tenant_role:tid===A?'admin':'member',membership_authz_version:1};}
  function paged(items,u){const current=Number(u.searchParams.get('page')||1), size=Number(u.searchParams.get('page_size')||20);return {items:items.slice((current-1)*size,current*size),total:items.length,page:current,page_size:size,total_pages:Math.ceil(items.length/size)};}
  await page.route('**/*',async route=>{
    const request=route.request(), url=new URL(request.url());
    if(url.origin!==base)return route.abort();
    if(!url.pathname.startsWith('/api/')){assert.ok(!request.url().includes(SECRET),'invitation leaked outside API');return route.continue();}
    const p=url.pathname, method=request.method(), token=(request.headers().authorization||'').replace(/^Bearer /,''), data=request.postDataJSON();
    const safePath=p.replace(/\/invitations\/[0-9a-f]{64}\/accept/,'/invitations/[redacted]/accept');
    state.calls.push({method,path:safePath,query:url.search,body:data});
    let body, status=200;
    if(p==='/api/v1/settings/public')body={site_name:'KeyCompute',distribution_enabled:false};
    else if(p==='/api/v1/auth/login')body=session((initial===null||initial==='mock-expired')?'mock-global':'mock-A');
    else if(p==='/api/v1/auth/refresh-token'){status=401;body={error:{message:'expired fixture session'}};}
    else if(p==='/api/v1/me'&&token==='mock-expired'){status=401;body={error:{message:'expired fixture session'}};}
    else if(p==='/api/v1/me')body=profile(token);
    else if(p==='/api/v1/me/tenant')body=session(data.tenant_id===A?'mock-A':data.tenant_id===B?'mock-B':'mock-global');
    else if(p===`/api/v1/invitations/${SECRET}/accept`){
      state.accepted=true;body={tenant:tenant(A),membership:member(U,'owner@fixture.invalid','member')};
    }else if(p===`/api/v1/tenants/${A}`||p===`/api/v1/tenants/${B}`){
      if(method==='PATCH'){assert.equal(data.expected_authz_version,state.configVersion);state.configVersion++;}
      body=tenant(p.endsWith(A)?A:B);
    }else if(p===`/api/v1/tenants/${A}/members`){
      const items=state.members.filter(m=>m.email.includes(url.searchParams.get('search')||''));
      if(state.holdMembers){state.seenMembers?.();await state.holdMembers;}
      body=paged(items,url);
    }else if(p.startsWith(`/api/v1/tenants/${A}/members/`)){
      const row=state.members.find(m=>m.user_id===p.split('/').at(-1));assert.ok(row);
      assert.equal(data.expected_authz_version,row.authz_version);
      if(method==='DELETE'){row.membership_status='removed';row.removed_at=stamp;}
      else if(method==='PATCH'){row.tenant_role=data.tenant_role||row.tenant_role;row.membership_status=data.status||row.membership_status;}
      row.authz_version++;body=row;
    }else if(p===`/api/v1/tenants/${A}/invitations`){
      if(method==='POST'){
        if(state.createUnavailable){status=503;body={error:{message:'uncertain fixture outcome'}};}
        else{const existed=state.invitation?.status==='pending';
          if(!existed)state.invitation={id:I,tenant_id:A,invited_by:U,email:data.email,tenant_role:data.tenant_role,status:'pending',expires_at:'2099-01-01T00:00:00Z',accepted_by:null,accepted_at:null,revoked_at:null,created_at:stamp,updated_at:stamp};
          body={invitation:state.invitation,outcome:existed?'already_pending':'created',notification:existed?'not_applicable':'unconfigured',acceptance_link:existed?null:`${base}/invite#token=${SECRET}`};}
      }else body=paged(state.invitation?[state.invitation]:[],url);
    }else if(p===`/api/v1/tenants/${A}/invitations/${I}/revoke`){state.invitation.status='revoked';state.invitation.revoked_at=stamp;body=state.invitation;}
    else if(p===`/api/v1/tenants/${A}/transfer-ownership`){state.owner=data.new_owner_user_id;state.configVersion++;body=tenant(A);}
    else if(p===`/api/v1/tenants/${A}/audit-events`)body=paged([{id:I,tenant_id:A,actor_user_id:U,action:'member.update',resource_type:'member',resource_id:P,request_id:Q,credential_kind:'jwt',platform_role:'none',tenant_role:'admin',metadata:{case:'safe metadata'},result:'success',created_at:stamp}],url);
    else {state.unknown.push(safePath);status=404;body={error:{message:'unexpected fixture endpoint'}};}
    await route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(body)}).catch(error=>{
      if(!state.holdMembers)throw error; // A canceled, deliberately delayed read may no longer have a receiver.
    });
  });
  latest={context,page,state}; return latest;
}
async function screenshot(page,name){await page.screenshot({path:path.join(out,`${name}.png`),fullPage:true});}
function noSecretStorage(page){return page.evaluate(secret=>![...Object.values(localStorage),...Object.values(sessionStorage)].some(value=>value.includes(secret)),SECRET);}
try{
  browser=await chromium.launch({headless:true,args:['--no-sandbox']});
  const f=await fixture();const {page,state}=f;
  await page.goto(base+'/tenant');await page.locator('#tenant-name').waitFor();
  await page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant A');
  assert.equal(await page.locator('#app-sidebar').getByRole('button',{name:'Members',exact:true}).count(),1);
  await screenshot(page,'workspace');
  await page.locator('nav a[href="/tenant/members"]').click();await page.getByText('member@fixture.invalid',{exact:true}).waitFor();
  const peer=page.locator('tr').filter({hasText:'member@fixture.invalid'});
  await peer.getByRole('button',{name:'Toggle admin / member'}).click();
  await page.getByRole('dialog').getByRole('button',{name:'Confirm',exact:true}).click();
  await page.waitForFunction(()=>[...document.querySelectorAll('tbody tr')].some(r=>r.textContent.includes('member@fixture.invalid')&&r.children[1].textContent==='admin'));
  assert.equal(state.members.find(m=>m.user_id===P).tenant_role,'admin');
  assert.equal(state.calls.filter(c=>c.method==='PATCH').length,1);
  await peer.getByRole('button',{name:'Suspend / restore member'}).click();
  await page.getByRole('dialog').getByRole('button',{name:'Confirm',exact:true}).click();
  await peer.getByText('suspended',{exact:true}).waitFor();
  await peer.getByRole('button',{name:'Remove member'}).click();
  await page.getByRole('dialog').getByRole('button',{name:'Confirm',exact:true}).click();
  await peer.getByText('removed',{exact:true}).waitFor();
  assert.equal(state.calls.find(c=>c.method==='DELETE').body.expected_authz_version,3);
  await page.locator('#member-search').fill('a&tenant_id='+B);
  await page.locator('.toolbar').getByRole('button',{name:'Search members',exact:true}).click();
  await page.getByText('No records',{exact:true}).waitFor();
  const searchCall=state.calls.filter(c=>c.path.endsWith('/members')&&c.query.includes('search=')).at(-1);
  assert.equal(new URLSearchParams(searchCall.query).get('tenant_id'),null);
  assert.equal(new URLSearchParams(searchCall.query).get('search'),'a&tenant_id='+B);
  await page.locator('nav a[href="/tenant/invitations"]').click();await page.locator('#invite-email').waitFor();
  await page.locator('#invite-email').fill('guest@fixture.invalid');
  await page.getByRole('button',{name:'Create invitation',exact:true}).click();
  await page.locator('#invite-recovery').waitFor();
  assert.equal(await noSecretStorage(page),true);
  await page.getByRole('button',{name:'Create invitation',exact:true}).click();
  await page.getByText('A pending invitation exists; no duplicate notification was sent.',{exact:true}).waitFor();
  assert.equal(await page.locator('#invite-recovery').count(),0);
  await page.getByRole('button',{name:'Revoke invitation',exact:true}).click();
  await page.getByRole('dialog').getByRole('button',{name:'Confirm',exact:true}).click();
  await page.locator('tbody').getByText('revoked',{exact:true}).waitFor();
  state.createUnavailable=true;
  await page.getByRole('button',{name:'Create invitation',exact:true}).click();
  await page.locator('[role="alert"]').first().waitFor();
  assert.equal(state.calls.filter(c=>c.method==='POST'&&c.path.endsWith('/invitations')).length,3);
  await page.locator('nav a[href="/tenant/audit"]').click();await page.getByText('member.update',{exact:true}).waitFor();
  await screenshot(page,'audit');
  await page.locator('nav a[href="/tenant"]').click();await page.locator('#tenant-name').waitFor();
  await page.locator('#tenant-name').fill('UNSAVED-TENANT-A');
  await page.locator('#workspace-select').selectOption(B);
  await page.getByRole('button',{name:'Switch workspace',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant B');
  assert.equal(await page.locator('#tenant-name').isDisabled(),true);
  assert.equal(await page.locator('nav a[href="/tenant/members"]').count(),0);
  assert.doesNotMatch(await page.locator('body').innerText(),/UNSAVED-TENANT-A|guest@fixture/);
  await page.goto(base+'/tenant/members');await page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();
  assert.equal(state.calls.filter(c=>c.path===`/api/v1/tenants/${B}/members`).length,0);
  await page.locator('a[href="/tenant"]').click();await page.locator('#workspace-select').waitFor();
  await page.locator('#workspace-select').selectOption('');
  await page.getByRole('button',{name:'Switch workspace',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('#workspace-select')?.value===''&&!document.querySelector('#tenant-name'));
  assert.equal(state.unknown.length,0,JSON.stringify(state.unknown));
  cases.push('restored workspace, member commands/revisions, literal search, invitations, audit, switch and global/member guards');
  await f.context.close();

  const invite=await fixture(null);
  await invite.page.goto(base+`/invite#token=${SECRET}`);
  await invite.page.getByText('Sign in with the invited email. You will return to this invitation after login.',{exact:true}).waitFor();
  await invite.page.waitForFunction(()=>getComputedStyle(document.documentElement).getPropertyValue('--bg-primary').trim().length>0);
  assert.equal(new URL(invite.page.url()).hash,'');assert.equal(await noSecretStorage(invite.page),true);
  assert.equal(invite.state.calls.filter(c=>c.path.endsWith('/accept')).length,0);
  await invite.page.locator('a[href="/auth/login"]').click();await invite.page.waitForURL(base+'/');
  await invite.page.getByRole('button',{name:'Sign In',exact:true}).first().click();
  const dialog=invite.page.getByRole('dialog');await dialog.locator('input[type="email"]').fill('owner@fixture.invalid');
  await dialog.locator('input[type="password"]').fill('fixture-password-only');
  await dialog.locator('button[type="submit"]').click();await invite.page.waitForURL(base+'/invite');
  const accept=invite.page.getByRole('button',{name:'Accept invitation',exact:true});await accept.waitFor();
  await accept.click();await invite.page.getByText('Invitation accepted. Select the tenant workspace you joined.',{exact:true}).waitFor();
  assert.equal(invite.state.calls.filter(c=>c.path.endsWith('/accept')).length,1);
  assert.equal(await accept.isDisabled(),true);assert.equal(await noSecretStorage(invite.page),true);
  await invite.page.waitForFunction(()=>getComputedStyle(document.documentElement).getPropertyValue('--bg-primary').trim().length>0);
  await screenshot(invite.page,'invitation-accepted');
  assert.equal(invite.state.unknown.length,0,JSON.stringify(invite.state.unknown));
  cases.push('fragment scrubbed before router; login resumes invite; explicit one-shot acceptance; no secret storage');
  await invite.context.close();

  for(const identity of ['mock-root','mock-op']){
    const f=await fixture(identity);await f.page.goto(base+'/tenant/members');
    await f.page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();
    assert.equal(f.state.calls.filter(c=>c.path.includes('/tenants/')).length,0);
    await f.context.close();
  }
  cases.push('root/operator global identities do not synthesize tenant membership');
  const expired=await fixture('mock-expired');
  await expired.page.goto(base+`/invite#token=${SECRET}`);
  await expired.page.getByText('Sign in with the invited email. You will return to this invitation after login.',{exact:true}).waitFor();
  assert.equal(new URL(expired.page.url()).hash,'');
  await expired.page.locator('a[href="/auth/login"]').click();await expired.page.waitForURL(base+'/');
  await expired.page.getByRole('button',{name:'Sign In',exact:true}).first().click();
  const login=expired.page.getByRole('dialog');
  await login.locator('input[type="email"]').fill('owner@fixture.invalid');
  await login.locator('input[type="password"]').fill('fixture-password-only');
  await login.locator('button[type="submit"]').click();
  await expired.page.waitForURL(base+'/invite');
  await expired.page.getByRole('button',{name:'Accept invitation',exact:true}).click();
  await expired.page.getByText('Invitation accepted. Select the tenant workspace you joined.',{exact:true}).waitFor();
  assert.equal(expired.state.calls.filter(c=>c.path.endsWith('/accept')).length,1);
  assert.deepEqual(expired.state.unknown,[]);
  await expired.context.close();
  cases.push('an expired restored credential does not erase the invitation before verified login');

  const config=await fixture();
  await config.page.goto(base+'/tenant');
  await config.page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant A');
  await config.page.locator('#tenant-name').fill('Changed by current tenant admin');
  await config.page.getByRole('button',{name:'Save changes',exact:true}).click();
  await config.page.getByRole('dialog').getByRole('button',{name:'Confirm',exact:true}).click();
  await config.page.waitForURL(base+'/');
  assert.equal(config.state.calls.filter(c=>c.method==='PATCH'&&c.path===`/api/v1/tenants/${A}`).length,1);
  assert.equal(config.state.configVersion,2);
  assert.equal(await config.page.evaluate(()=>localStorage.getItem('keyc_access_token')),null);
  assert.deepEqual(config.state.unknown,[]);
  await config.context.close();
  cases.push('configuration save sends the displayed revision and clears the invalidated selected session');

  const transfer=await fixture();
  await transfer.page.goto(base+'/tenant/members');
  const targetAdmin=transfer.page.locator('tr').filter({hasText:'admin@fixture.invalid'});
  await targetAdmin.getByRole('button',{name:'Transfer ownership',exact:true}).click();
  await transfer.page.getByRole('dialog').getByRole('button',{name:'Confirm',exact:true}).click();
  await transfer.page.waitForURL(base+'/');
  assert.equal(transfer.state.owner,Q);
  assert.equal(transfer.state.calls.filter(c=>c.path.endsWith('/transfer-ownership')).length,1);
  assert.equal(await transfer.page.evaluate(()=>localStorage.getItem('keyc_access_token')),null);
  assert.deepEqual(transfer.state.unknown,[]);
  await transfer.context.close();
  cases.push('ownership transfer explicitly selects an active admin and invalidates the selected session');
  assert.deepEqual(pageErrors,[]);
  const report={passed:true,basis:'actual compiled WASM and synthetic intercepted HTTP; no production or backend security claims',cases,page_errors:pageErrors};
  await fs.writeFile(path.join(out,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
 }catch(error){
  if(latest && !latest.page.isClosed()){
    await screenshot(latest.page,'failure').catch(()=>{});
    const diagnostic={url:latest.page.url().split('#')[0],text:(await latest.page.locator('body').innerText()).replaceAll(SECRET,'[redacted]'),fields:await latest.page.locator('input').evaluateAll(nodes=>nodes.filter(n=>n.type!=='password').map(n=>({id:n.id,value:n.value.replace(/[0-9a-f]{64}/g,'[redacted]')}))),errors:pageErrors,last_calls:latest.state.calls.slice(-12),unknown:latest.state.unknown};
    await fs.writeFile(path.join(out,'failure.json'),JSON.stringify(diagnostic,null,2));
    console.error(JSON.stringify(diagnostic,null,2));
  }
  console.error(error);process.exitCode=1;
}
finally{if(browser)await browser.close();await new Promise(resolve=>server.close(resolve));}
