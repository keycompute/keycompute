// Real compiled WASM; all business requests are intercepted synthetic fixtures.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const {chromium}=await import(process.env.KC_PLAYWRIGHT_MODULE||'playwright');
const root=path.resolve(process.env.KC_WEB_ROOT||'target/dx/web/release/web/public');
const output=path.resolve(process.env.KC_BROWSER_OUTPUT||'tenant-keys-browser-results');
await fs.mkdir(output,{recursive:true});
const A='11111111-1111-4111-8111-111111111111', B='22222222-2222-4222-8222-222222222222';
const U='33333333-3333-4333-8333-333333333333', V='44444444-4444-4444-8444-444444444444';
const K='55555555-5555-4555-8555-555555555555', J='66666666-6666-4666-8666-666666666666';
const I='77777777-7777-4777-8777-777777777777', N='88888888-8888-4888-8888-888888888888';
const stamp='2026-09-24T00:00:00Z', future='2030-01-01T00:00:00+00:00', secret='sk-'+ 'T'.repeat(48);
const server=http.createServer(async(req,res)=>{try{
    let file=path.resolve(root,'.'+decodeURIComponent(new URL(req.url,'http://localhost').pathname));
    if(file!==root&&!file.startsWith(root+path.sep))throw Error('invalid asset');
    let data;try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}
    const mime={'.html':'text/html','.js':'text/javascript','.wasm':'application/wasm','.css':'text/css','.woff2':'font/woff2','.png':'image/png','.jpg':'image/jpeg','.svg':'image/svg+xml','.ico':'image/x-icon'};
    res.writeHead(200,{'Content-Type':mime[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
}catch{res.writeHead(500);res.end();}});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
let currentPage;const errors=[],cases=[];
const key=(id=K,owner=V)=>({id,tenant_id:A,owner_user_id:owner,name:owner===V?'Peer key':'My key',key_preview:'sk-fixt****',revoked:false,revoked_at:null,expires_at:future,last_used_at:null,created_at:stamp,updated_at:stamp});
const intent=(owner=U,replace=null)=>({id:I,tenant_id:A,owner_user_id:owner,requested_by_user_id:V,replaces_key_id:replace,requested_name:'Owner issued key',requested_expires_at:null,status:'pending',expires_at:future,claimed_at:null,created_key_id:null,created_at:stamp});
async function fixture({selected=A,role='none',admin=true,user=U,pending=true}={}){
    const state={selected,role,admin,user,keys:[key(),key(J,U)],intents:pending?[intent()]:[],calls:[],unknown:[],foreign:false,unavailable:false,conflict:false,hold:false,release:null,retained:true,revision:0,clipboard:[]};
    const context=await browser.newContext({viewport:{width:1440,height:1000}});
    await context.addInitScript(()=>{localStorage.setItem('keyc_access_token','fixture-token');localStorage.setItem('keyc_lang','en');});
    const page=await context.newPage();page.setDefaultTimeout(20000);currentPage=page;page.on('pageerror',e=>errors.push(e.message));
    const profile=()=>({id:state.user,user_id:state.user,email:'key-owner@fixture.invalid',name:'Key reviewer',platform_role:state.role,status:'active',created_at:stamp,
        selected_tenant:state.selected?{id:state.selected,name:state.selected===A?'Tenant A':'Tenant B',slug:'fixture',tenant_role:state.selected===A&&state.admin?'admin':'member',authz_version:1,membership_authz_version:1}:null,
        memberships:[{tenant_id:A,tenant_name:'Tenant A',tenant_role:state.admin?'admin':'member',status:'active',authz_version:1},{tenant_id:B,tenant_name:'Tenant B',tenant_role:'member',status:'active',authz_version:1}],
        capabilities:{platform:state.role==='root'?['users:manage']:state.role==='operator'?['platform:tenant_health']:[],tenant:state.selected===A&&state.admin?['tenant:manage']:[]}});
    await page.route('**/*',async route=>{
        const r=route.request(),url=new URL(r.url());if(url.origin!==base)return route.abort();if(!url.pathname.startsWith('/api/')&&url.pathname!=='/v1/models')return route.continue();
        const p=url.pathname,m=r.method(),body=r.postDataJSON();state.calls.push({path:p,method:m,query:url.search,body});
        const answer=(v,status=200)=>route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(v)});
        const paged=(rows,field)=>{const n=Number(url.searchParams.get('page')||1),size=Number(url.searchParams.get('page_size')||20);return {[field]:rows.slice((n-1)*size,n*size),total:rows.length,page:n,page_size:size,total_pages:Math.ceil(rows.length/size)};};
        const touch=row=>{state.revision++;row.updated_at=`2026-09-24T00:00:${String(state.revision).padStart(2,'0')}Z`;};
        if(p==='/v1/models')return answer({object:'list',data:[]});
        if(p==='/api/v1/settings/public')return answer({site_name:'KeyCompute',distribution_enabled:false});
        if(p==='/api/v1/me'&&m==='GET')return answer(profile());
        if(p==='/api/v1/me/tenant'){state.selected=body.tenant_id;return answer({...profile(),access_token:'fixture-selected',token_type:'Bearer',expires_in:3600});}
        if(p===`/api/v1/tenants/${state.selected}`)return answer({id:state.selected,owner_user_id:U,name:state.selected===A?'Tenant A':'Tenant B',slug:'fixture',description:null,status:'active',default_rpm_limit:60,default_tpm_limit:100000,authz_version:1,tenant_role:state.selected===A?'admin':'member',membership_authz_version:1});
        const prefix=`/api/v1/tenants/${A}`;
        if(p===`${prefix}/members`)return answer(paged([U,V].map(id=>({user_id:id,email:`${id===U?'owner':'peer'}@fixture.invalid`,name:null,user_status:'active',tenant_role:id===U?'admin':'member',membership_status:'active',authz_version:1,invited_by:null,joined_at:stamp,removed_at:null})),'items'));
        if(p===`${prefix}/keys`&&m==='GET'){
            const owner=url.searchParams.get('owner_user_id');let rows=state.keys.filter(k=>(!owner||k.owner_user_id===owner)&&(url.searchParams.get('include_revoked')==='true'||!k.revoked));
            if(state.foreign)rows=rows.map(k=>({...k,tenant_id:B}));return answer(paged(rows,'keys'));
        }
        if(p===`${prefix}/key-issuance`&&m==='GET'){const owner=url.searchParams.get('owner_user_id');return answer(paged(state.intents.filter(i=>i.status==='pending'&&(!owner||i.owner_user_id===owner)),'intents'));}
        if(p.startsWith(prefix)&&m!=='GET'){
            if(state.unavailable)return answer({error:{message:'uncertain command'}},503);
            if(state.conflict)return answer({error:{message:'version conflict'}},409);
            if(state.hold)await new Promise(resolve=>{state.release=resolve;});
            assert.equal(body?.tenant_id,undefined);assert.equal(body?.platform_role,undefined);
            if(p===`${prefix}/keys/issuance`){const created={...intent(body.owner_user_id),id:N,requested_by_user_id:state.user,requested_name:body.name,requested_expires_at:body.expires_at??null};state.intents.push(created);return answer({intent:created,outcome:'created',message:'owner_claim_required'},201);}
            const parts=p.slice(prefix.length+1).split('/'),id=parts[1];
            if(parts[0]==='key-issuance'&&parts[2]==='cancel'){const row=state.intents.find(i=>i.id===id);row.status='cancelled';return answer({intent:row,outcome:'cancelled',message:'cancelled'});}
            const row=state.keys.find(k=>k.id===id);if(!row)return answer({error:{message:'missing key'}},404);
            if(m==='PATCH'){assert.equal(body.expected_updated_at,row.updated_at);if(body.name!==undefined)row.name=body.name;if('expires_at' in body)row.expires_at=body.expires_at;touch(row);return answer(row);}
            if(parts[2]==='rotate'){const created={...intent(row.owner_user_id,row.id),requested_name:body.name,requested_expires_at:body.expires_at??null};state.intents=[...state.intents.filter(i=>i.id!==I),created];return answer({intent:created,outcome:'created',message:'owner_claim_required'},201);}
            if(parts[2]==='revoke'||m==='DELETE'){row.revoked=true;row.revoked_at=stamp;touch(row);const deleted=m==='DELETE'&&!state.retained;if(deleted)state.keys=state.keys.filter(k=>k.id!==id);return answer({success:true,key:deleted?null:row,key_id:id,revoked_at:stamp,deleted});}
        }
        if(p==='/api/v1/me/key-issuance'&&m==='GET'){
            assert.equal(url.searchParams.has('owner_user_id'),false);assert.equal(url.searchParams.has('tenant_id'),false);
            let rows=state.intents.filter(i=>i.status==='pending'&&i.owner_user_id===state.user);if(state.foreign)rows=rows.map(i=>({...i,owner_user_id:V}));return answer(paged(rows,'intents'));
        }
        if(p.startsWith('/api/v1/me/key-issuance/')&&m==='POST'){
            assert.deepEqual(body,{});if(state.unavailable)return answer({error:{message:`reflected ${secret}`}},503);
            if(state.hold)await new Promise(resolve=>{state.release=resolve;});
            const id=p.split('/')[5],row=state.intents.find(i=>i.id===id&&i.owner_user_id===state.user&&i.status==='pending');
            if(!row)return answer({error:{message:'not current owner request'}},409);
            if(p.endsWith('/decline')){row.status='cancelled';return answer({intent:row,outcome:'declined',message:'owner_declined'});}
            row.status='claimed';row.created_key_id=N;row.claimed_at=stamp;
            if(row.replaces_key_id){const prior=state.keys.find(k=>k.id===row.replaces_key_id);prior.revoked=true;prior.revoked_at=stamp;}
            state.keys.push({...key(N,state.user),name:row.requested_name,expires_at:row.requested_expires_at});
            return answer({outcome:'claimed',intent_id:row.id,key_id:N,name:row.requested_name,expires_at:row.requested_expires_at,created_at:stamp,key:secret,secret_returned_once:true});
        }
        if(p==='/api/v1/keys'&&m==='POST'){
            assert.equal(body.expires_at,undefined);assert.equal(body.tenant_id,undefined);assert.equal(body.owner_user_id,undefined);
            if(state.hold)await new Promise(resolve=>{state.release=resolve;});
            state.keys.push({...key(N,state.user),name:body.name});
            return answer({success:true,message:'API Key created',key:secret,key_id:N,name:body.name,created_at:stamp,expires_at:future,never_expires:false});
        }
        if(p==='/api/v1/keys'&&m==='GET')return answer(paged(state.keys.filter(k=>k.owner_user_id===state.user&&!k.revoked).map(k=>({id:k.id,name:k.name,key_preview:k.key_preview,created_at:k.created_at,last_used_at:null,is_active:true,expires_at:k.expires_at})),'keys'));
        state.unknown.push(`${m} ${p}`);return answer({error:{message:'unexpected fixture endpoint'}},404);
    });
    return {state,page,context};
}
const peerRow=page=>page.locator('.tenant-key-admin tbody tr').filter({hasText:V}).first();
async function open(page,name){await peerRow(page).getByRole('button',{name,exact:true}).click();const d=page.getByRole('dialog');await d.waitFor();return d;}
async function confirm(d){await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.waitFor({state:'detached'});}
async function switchWorkspace(page){await page.evaluate(()=>{history.pushState(null,'','/tenant');window.dispatchEvent(new PopStateEvent('popstate'));});await page.locator('#workspace-select').waitFor();await page.locator('#workspace-select').selectOption(B);await page.getByRole('button',{name:'Switch workspace',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant B');}
async function noSecret(page){assert.doesNotMatch(await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage})),/sk-T{48}/);}
try{
    let {state,page,context}=await fixture({pending:false});await page.goto(base+'/tenant/keys');await peerRow(page).waitFor();
    let d=await open(page,'Edit key metadata');assert.equal(await d.locator('#key-request-owner').isDisabled(),true);await d.locator('#key-request-name').fill('Renamed peer');await confirm(d);
    const patch=state.calls.find(c=>c.method==='PATCH');assert.equal('expires_at' in patch.body,false);assert.equal(patch.body.expected_updated_at,stamp);
    d=await open(page,'Edit key metadata');await d.locator('#key-expiry-mode').selectOption('never');await confirm(d);assert.equal(state.keys.find(k=>k.id===K).expires_at,null);
    d=await open(page,'Edit key metadata');await d.locator('#key-expiry-mode').selectOption('at');await d.locator('#key-expiry-time').fill('2000-01-01T00:00:00Z');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();assert.equal(state.calls.filter(c=>c.method==='PATCH').length,2);
    await d.locator('#key-expiry-time').fill(future);await confirm(d);
    d=await open(page,'Request key rotation');await confirm(d);assert.equal(state.keys.find(k=>k.id===K).revoked,false);
    await page.getByRole('button',{name:'Pending issuance',exact:true}).click();await peerRow(page).waitFor();assert.equal(await page.getByRole('button',{name:'Claim my key',exact:true}).count(),0);
    d=await open(page,'Cancel issuance request');await confirm(d);assert.equal(state.keys.find(k=>k.id===K).revoked,false);
    await page.getByRole('button',{name:'Request a new key',exact:true}).click();d=page.getByRole('dialog');await d.locator('#key-owner-option option[value="'+V+'"]').waitFor({state:'attached'});await d.locator('#key-owner-option').selectOption(V);await d.locator('#key-request-name').fill('New peer request');await d.locator('#key-expiry-mode').selectOption('never');await confirm(d);
    assert.equal(state.intents.find(i=>i.id===N).owner_user_id,V);assert.equal(state.keys.length,2);assert.equal(state.calls.filter(c=>c.path.endsWith('/claim')).length,0);
    await page.getByRole('button',{name:'Key metadata',exact:true}).click();await peerRow(page).waitFor();
    state.conflict=true;d=await open(page,'Edit key metadata');await d.locator('#key-request-name').fill('Stale edit');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();assert.equal(state.keys.find(k=>k.id===K).name,'Renamed peer');await d.getByRole('button',{name:'Cancel',exact:true}).click();state.conflict=false;
    state.unavailable=true;d=await open(page,'Edit key metadata');await d.locator('#key-request-name').fill('Uncertain');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();const count=state.calls.filter(c=>c.method==='PATCH').length;await page.waitForTimeout(150);assert.equal(state.calls.filter(c=>c.method==='PATCH').length,count);await d.getByRole('button',{name:'Cancel',exact:true}).click();state.unavailable=false;
    d=await open(page,'Remove key');await confirm(d);await page.getByText('Key revoked, not physically deleted: existing records retain its identity.',{exact:true}).waitFor();assert.equal(state.keys.length,2);assert.equal(state.keys.find(k=>k.id===K).revoked,true);
    await page.getByRole('checkbox',{name:'Include revoked keys',exact:true}).check();await peerRow(page).waitFor();assert.equal(await peerRow(page).getByRole('button',{name:'Request key rotation',exact:true}).isDisabled(),true);
    state.retained=false;d=await open(page,'Remove key');await confirm(d);assert.equal(state.keys.length,1);
    await page.locator('#tenant-key-owner').fill('invalid&tenant_id='+B);const n=state.calls.length;await page.getByRole('button',{name:'Apply key owner filter',exact:true}).click();await page.locator('.tenant-key-admin > [role="alert"]').waitFor();assert.equal(state.calls.length,n);
    await page.screenshot({path:path.join(output,'key-administration.png'),fullPage:true});await noSecret(page);assert.deepEqual(state.unknown,[]);cases.push('admin metadata CAS, explicit expiry, inert issuance/rotation, owner picker, retained revocation and no secret/automatic retry');await context.close();
    ({state,page,context}=await fixture({admin:false}));state.intents=[intent(U,J)];await page.goto(base+'/api-keys/issuance');await page.getByRole('button',{name:'Claim my key',exact:true}).waitFor();assert.equal(state.calls.filter(c=>c.path.endsWith('/claim')).length,0);
    await page.getByRole('button',{name:'Claim my key',exact:true}).click();d=page.getByRole('dialog');await confirm(d);await page.locator('.owner-issued-secret pre').waitFor();assert.equal(await page.locator('.owner-issued-secret pre').innerText(),secret);assert.equal(state.keys.find(k=>k.id===J).revoked,true);await noSecret(page);
    await page.evaluate(()=>{window.__clipboardValues=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>{window.__clipboardValues.push(text);}}});});await page.getByRole('button',{name:'Copy my key',exact:true}).click();await page.getByText('Key copied.',{exact:true}).waitFor();assert.deepEqual(await page.evaluate(()=>window.__clipboardValues),[secret]);
    await page.evaluate(()=>{Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async()=>{throw new Error('denied');}}});});await page.getByRole('button',{name:'Copy my key',exact:true}).click();await page.getByText('Clipboard access failed. Select and copy the displayed key manually.',{exact:true}).waitFor();assert.equal(await page.getByText('Key copied.',{exact:true}).count(),0);
    await page.getByRole('button',{name:'Saved securely — hide key',exact:true}).click();await page.locator('.owner-issued-secret').waitFor({state:'detached'});await noSecret(page);
    state.intents=[{...intent(),id:'99999999-9999-4999-8999-999999999999'}];await page.getByRole('button',{name:'Refresh data',exact:true}).click();await page.getByRole('button',{name:'Decline my request',exact:true}).click();d=page.getByRole('dialog');await confirm(d);assert.equal(state.intents[0].status,'cancelled');assert.equal(state.calls.filter(c=>c.path.endsWith('/claim')).length,1);
    assert.deepEqual(state.unknown,[]);cases.push('owner-only explicit claim/rotation, secret memory and hide, real clipboard Promise outcomes, decline');await context.close();
    ({state,page,context}=await fixture({admin:false}));state.unavailable=true;await page.goto(base+'/api-keys/issuance');await page.getByRole('button',{name:'Claim my key',exact:true}).click();d=page.getByRole('dialog');await confirm(d);await page.locator('.owner-key-issuance > [role="alert"]').waitFor();assert.doesNotMatch(await page.locator('body').innerText(),/sk-T{48}/);assert.equal(state.calls.filter(c=>c.path.endsWith('/claim')).length,1);await noSecret(page);state.unavailable=false;state.hold=true;
    await page.getByRole('button',{name:'Claim my key',exact:true}).click();d=page.getByRole('dialog');await d.getByRole('button',{name:'Confirm',exact:true}).click();for(let i=0;i<200&&!state.release;i++)await page.waitForTimeout(10);assert.ok(state.release);await page.keyboard.press('Escape');assert.equal(await d.isVisible(),true);await switchWorkspace(page);state.release();await page.waitForTimeout(150);assert.doesNotMatch(await page.locator('body').innerText(),/sk-T{48}/);await noSecret(page);assert.equal(state.calls.filter(c=>c.path.endsWith('/claim')).length,2);assert.deepEqual(state.unknown,[]);cases.push('secret reflection sanitized; pending claim cannot publish or replay in another workspace');await context.close();
    ({state,page,context}=await fixture({admin:false,pending:false}));await page.goto(base+'/api-keys');await page.getByRole('button',{name:'Create API Key',exact:true}).waitFor();
    await page.getByRole('button',{name:'Create API Key',exact:true}).click();d=page.getByRole('dialog');await d.getByPlaceholder('Name this key').fill('Direct personal key');await d.getByRole('button',{name:'Create',exact:true}).click();await page.getByText('API Key created',{exact:true}).waitFor();assert.match(await page.locator('.kc-api-success-panel').innerText(),/sk-T{48}/);await noSecret(page);
    await page.locator('.kc-api-actions').getByRole('link',{name:'My pending key requests',exact:true}).click();await page.locator('.owner-key-issuance').waitFor();assert.doesNotMatch(await page.locator('body').innerText(),/sk-T{48}/);await noSecret(page);
    await page.locator('.owner-key-issuance').getByRole('link',{name:'API Keys',exact:true}).click();await page.getByRole('button',{name:'Create API Key',exact:true}).waitFor();assert.equal(await page.locator('.kc-api-success-panel').count(),0);
    state.hold=true;await page.getByRole('button',{name:'Create API Key',exact:true}).click();d=page.getByRole('dialog');await d.getByPlaceholder('Name this key').fill('Delayed personal key');await d.getByRole('button',{name:'Create',exact:true}).click();for(let i=0;i<200&&!state.release;i++)await page.waitForTimeout(10);assert.ok(state.release);await switchWorkspace(page);state.release();await page.waitForTimeout(150);assert.doesNotMatch(await page.locator('body').innerText(),/sk-T{48}|Delayed personal key/);await noSecret(page);assert.equal(state.calls.filter(c=>c.path==='/api/v1/keys'&&c.method==='POST').length,2);assert.deepEqual(state.unknown,[]);cases.push('direct personal key creation retains its lifetime contract and clears one-time state on route/workspace change');await context.close();
    for(const config of [{selected:null,role:'root'},{selected:null,role:'operator'},{admin:false}]){({state,page,context}=await fixture(config));await page.goto(base+'/tenant/keys');await page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(state.calls.filter(c=>c.path.includes('/keys')||c.path.includes('/key-issuance')).length,0);await context.close();}
    ({state,page,context}=await fixture({admin:false}));state.foreign=true;await page.goto(base+'/api-keys/issuance');await page.locator('.owner-key-issuance [role="alert"]').waitFor();assert.equal(await page.getByRole('button',{name:'Claim my key',exact:true}).count(),0);await context.close();
    ({state,page,context}=await fixture({pending:false}));await page.goto(base+'/tenant/keys');await peerRow(page).waitFor();await page.setViewportSize({width:390,height:844});d=await open(page,'Edit key metadata');const bounds=await d.boundingBox();assert.ok(bounds&&bounds.x>=0&&bounds.x+bounds.width<=391);assert.equal(await d.evaluate(el=>el.contains(document.activeElement)),true);await page.screenshot({path:path.join(output,'key-editor-mobile.png'),fullPage:true});await page.keyboard.press('Escape');await d.waitFor({state:'detached'});assert.deepEqual(state.unknown,[]);cases.push('root/operator/member gates, foreign owner rejection and focused narrow-viewport editor');await context.close();
    assert.deepEqual(errors,[]);const report={passed:true,basis:'compiled WASM with synthetic HTTP; real SDK/server isolation tests run separately',cases,page_errors:errors};await fs.writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(error){if(currentPage&&!currentPage.isClosed()){console.error((await currentPage.locator('body').innerText()).replaceAll(secret,'[fixture secret redacted]').slice(0,9000));await currentPage.screenshot({path:path.join(output,'failure.png'),fullPage:true}).catch(()=>{});}console.error(error);process.exitCode=1;}
finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
