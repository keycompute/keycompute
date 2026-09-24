// Production-compiled WASM with synthetic intercepted HTTP, never real credentials/upstreams.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.KC_PLAYWRIGHT_MODULE || 'playwright');
const root=path.resolve(process.env.KC_WEB_ROOT || 'target/dx/web/release/web/public');
const output=path.resolve(process.env.KC_BROWSER_OUTPUT || 'tenant-responses-browser-results');
await fs.mkdir(output,{recursive:true});
const A='11111111-1111-4111-8111-111111111111', B='22222222-2222-4222-8222-222222222222';
const U='33333333-3333-4333-8333-333333333333', V='44444444-4444-4444-8444-444444444444';
const stamp='2026-09-23T00:00:00Z', id='same-resource-id';
const server=http.createServer(async(req,res)=>{try{
    const pathname=decodeURIComponent(new URL(req.url,'http://localhost').pathname);
    let file=path.resolve(root,'.'+pathname);if(file!==root&&!file.startsWith(root+path.sep))throw Error('invalid asset path');
    let data;try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}
    const mime={'.html':'text/html','.js':'text/javascript','.wasm':'application/wasm','.css':'text/css','.woff2':'font/woff2','.png':'image/png','.jpg':'image/jpeg','.svg':'image/svg+xml','.ico':'image/x-icon'};
    res.writeHead(200,{'Content-Type':mime[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
}catch{res.writeHead(500);res.end();}});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
let currentPage;const errors=[],cases=[];
const response=(owner,mode)=>({id,tenant_id:A,owner_user_id:owner,mode,provider:null,account_id:null,model:`${mode}-${owner===U?'owner-U':'owner-V'}`,status:'in_progress',background:true,store_response:true,stream:false,previous_response_id:null,conversation_id:null,revision:7,created_at:stamp,updated_at:stamp,expires_at:'2030-01-01T00:00:00Z',deleted:false,local_content_available:true,native_content_available:false});
const conversation=(owner,mode)=>({id,tenant_id:A,owner_user_id:owner,mode,account_id:null,model:`${mode}-${owner===U?'owner-U':'owner-V'}`,metadata:{case:`${mode}-${owner===U?'owner-U':'owner-V'}`},active_response_id:null,revision:7,created_at:stamp,updated_at:stamp,expires_at:'2030-01-01T00:00:00Z',deleted:false});
async function fixture({selected=A,role='none',admin=true}={}){
    const state={selected,role,admin,calls:[],unknown:[],foreign:false,unavailable:false,conflict:false,hold:false,release:null,reverse:false,
        responses:['passthrough','node_dispatch'].flatMap(mode=>[response(U,mode),response(V,mode)]),
        conversations:['passthrough','node_dispatch'].flatMap(mode=>[conversation(U,mode),conversation(V,mode)]),items:new Map()};
    for(const row of state.conversations)state.items.set(`${row.mode}:${row.owner_user_id}`,Array.from({length:21},(_,n)=>({id:`item-${n+1}`,role:'user',content:`${row.mode}-${row.owner_user_id===U?'owner-U':'owner-V'}-item-${n+1}`})));
    const context=await browser.newContext({viewport:{width:1440,height:1000}});
    await context.addInitScript(()=>{localStorage.setItem('keyc_access_token','fixture-token');localStorage.setItem('keyc_lang','en');});
    const page=await context.newPage();page.setDefaultTimeout(20000);currentPage=page;page.on('pageerror',e=>errors.push(e.message));
    const profile=()=>({id:U,user_id:U,email:'resource-owner@fixture.invalid',name:'Resource reviewer',platform_role:state.role,status:'active',created_at:stamp,
        selected_tenant:state.selected?{id:state.selected,name:state.selected===A?'Tenant A':'Tenant B',slug:'fixture',tenant_role:state.selected===A&&state.admin?'admin':'member',authz_version:1,membership_authz_version:1}:null,
        memberships:[{tenant_id:A,tenant_name:'Tenant A',tenant_role:state.admin?'admin':'member',status:'active',authz_version:1},{tenant_id:B,tenant_name:'Tenant B',tenant_role:'member',status:'active',authz_version:1}],
        capabilities:{platform:state.role==='root'?['users:manage']:state.role==='operator'?['platform:tenant_health']:[],tenant:state.selected===A&&state.admin?['tenant:manage']:[]}});
    await page.route('**/*',async route=>{
        const request=route.request(),url=new URL(request.url());if(url.origin!==base)return route.abort();
        if(!url.pathname.startsWith('/api/'))return route.continue();
        const p=url.pathname,m=request.method(),body=request.postDataJSON();state.calls.push({path:p,method:m,query:url.search,body});
        const answer=(value,status=200)=>route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(value)});
        if(p==='/api/v1/settings/public')return answer({site_name:'KeyCompute',distribution_enabled:false});
        if(p==='/api/v1/me'&&m==='GET')return answer(profile());
        if(p==='/api/v1/me/tenant'){state.selected=body.tenant_id;return answer({...profile(),access_token:'fixture-selected',token_type:'Bearer',expires_in:3600});}
        if(p===`/api/v1/tenants/${state.selected}`)return answer({id:state.selected,owner_user_id:U,name:state.selected===A?'Tenant A':'Tenant B',slug:'fixture',description:null,status:'active',default_rpm_limit:60,default_tpm_limit:100000,authz_version:1,tenant_role:state.selected===A?'admin':'member',membership_authz_version:1});
        const prefix=`/api/v1/tenants/${A}/`;
        if(p.startsWith(prefix)){
            const parts=p.slice(prefix.length).split('/').map(decodeURIComponent),kind=parts[0];
            if(['responses','conversations'].includes(kind)){
                if(parts.length===1&&m==='GET'){
                    const mode=url.searchParams.get('mode'),owner=url.searchParams.get('owner_user_id'),n=Number(url.searchParams.get('page')||1);
                    let rows=state[kind].filter(r=>!r.deleted&&r.mode===mode&&(!owner||r.owner_user_id===owner));
                    if(state.reverse)rows=rows.toReversed();
                    if(state.foreign)rows=rows.map(r=>({...r,tenant_id:B}));
                    return answer({items:rows.slice((n-1)*20,n*20),total:rows.length,page:n,page_size:20,total_pages:Math.ceil(rows.length/20)});
                }
                const [_,mode,owner,rid,collection,itemId]=parts;
                const row=state[kind].find(r=>r.mode===mode&&r.owner_user_id===owner&&r.id===rid&&!r.deleted);
                if(!row)return answer({error:{message:'fixture resource missing'}},404);
                if(m!=='GET'){
                    if(state.unavailable)return answer({error:{message:'uncertain mutation; refresh records'}},503);
                    if(state.conflict||body?.expected_revision!==row.revision)return answer({error:{message:'resource changed; refresh records'}},409);
                    if(state.hold)await new Promise(resolve=>{state.release=resolve;});
                    assert.equal(body.tenant_id,undefined);assert.equal(body.owner_user_id,undefined);
                }
                if(!collection&&m==='GET'){
                    if(kind==='responses')return answer({summary:row,response:{id:rid,object:'response',status:row.status,output:[{content:`${mode}-${owner===U?'owner-U':'owner-V'}-private <img src=x onerror="window.__resourceXss=1">`}]}});
                    return answer({summary:row,conversation:{id:rid,object:'conversation',created_at:1,metadata:row.metadata}});
                }
                if(m==='GET'&&['items','input_items'].includes(collection)){
                    let items=kind==='conversations'?state.items.get(`${mode}:${owner}`):[{id:'input-one',role:'user',content:`${mode}-${owner===U?'owner-U':'owner-V'}-input`}];
                    if(url.searchParams.get('order')!=='asc')items=items.toReversed();
                    const after=url.searchParams.get('after'),start=after?items.findIndex(i=>i.id===after)+1:0;
                    const limit=Number(url.searchParams.get('limit')||20),data=items.slice(start,start+limit);
                    return answer({object:'list',data,first_id:data[0]?.id??null,last_id:data.at(-1)?.id??null,has_more:start+limit<items.length});
                }
                if(!collection&&m==='PATCH'&&kind==='conversations'){row.metadata=body.metadata;row.revision++;return answer({id:rid,object:'conversation',metadata:row.metadata,created_at:1});}
                if(!collection&&m==='DELETE'){row.deleted=true;row.revision++;return answer({id:rid,object:kind==='responses'?'response':'conversation',deleted:true});}
                if(collection==='cancel'&&m==='POST'){row.status='cancelled';row.revision++;return answer({id:rid,object:'response',status:row.status});}
                if(collection==='items'&&kind==='conversations'){
                    const key=`${mode}:${owner}`;row.revision++;
                    if(m==='POST'){state.items.get(key).push(...body.items.map((item,i)=>({...item,id:`added-${row.revision}-${i}`})));return answer({id:rid,object:'conversation',metadata:row.metadata,created_at:1});}
                    if(m==='DELETE'&&itemId){state.items.set(key,state.items.get(key).filter(i=>i.id!==itemId));return answer({id:itemId,object:'conversation.item',deleted:true});}
                }
            }
        }
        state.unknown.push(`${m} ${p}`);return answer({error:{message:'unexpected fixture endpoint'}},404);
    });
    return {state,page,context};
}
const rowFor=(page,owner)=>page.locator('.tenant-response-admin tbody tr').filter({hasText:owner});
async function open(page,owner,name){await rowFor(page,owner).getByRole('button',{name,exact:true}).click();const d=page.getByRole('dialog');await d.waitFor();return d;}
async function confirm(d){await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.waitFor({state:'detached'});}
try{
    let {state,page,context}=await fixture();await page.goto(base+'/tenant/responses');await rowFor(page,V).waitFor();
    assert.equal(await page.locator('#managed-resource-mode option').count(),2);
    let d=await open(page,V,'Inspect resource content');await d.getByText(/passthrough-owner-V-private/).waitFor();assert.doesNotMatch(await d.innerText(),/owner-U-private/);
    assert.equal(await page.evaluate(()=>window.__resourceXss),undefined);assert.doesNotMatch(await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage})),/owner-V-private/);await d.getByRole('button',{name:'Close',exact:true}).click();
    // Reordering rows with identical opaque IDs must not reuse another owner's callback.
    state.reverse=true;await page.locator('.tenant-response-admin').getByRole('button',{name:'Refresh data',exact:true}).click();await rowFor(page,U).waitFor();
    d=await open(page,U,'Inspect resource content');await d.getByText(/passthrough-owner-U-private/).waitFor();await d.getByRole('button',{name:'Close',exact:true}).click();
    d=await open(page,V,'Request response cancellation');await confirm(d);await rowFor(page,V).getByText('cancelled',{exact:true}).waitFor();
    assert.equal(state.responses.find(r=>r.mode==='passthrough'&&r.owner_user_id===U).status,'in_progress');
    await page.getByRole('button',{name:'Conversations',exact:true}).click();await rowFor(page,V).waitFor();
    d=await open(page,U,'Edit conversation metadata');await d.locator('#resource-content-json').fill('{"secret":"unsaved-U-draft"}');await d.getByRole('button',{name:'Cancel',exact:true}).click();
    d=await open(page,V,'Edit conversation metadata');assert.match(await d.locator('#resource-content-json').inputValue(),/owner-V/);assert.doesNotMatch(await d.innerText(),/unsaved-U/);
    await d.locator('#resource-content-json').fill('{"case":5}');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();assert.equal(state.calls.filter(c=>c.method==='PATCH').length,0);
    await d.locator('#resource-content-json').fill('{"case":"changed-V"}');await confirm(d);assert.equal(state.conversations.find(r=>r.mode==='passthrough'&&r.owner_user_id===U).metadata.case,'passthrough-owner-U');
    d=await open(page,V,'Inspect items');await d.getByText(/owner-V-item-21/).waitFor();await d.getByRole('button',{name:'Next',exact:true}).click();await d.getByText(/owner-V-item-1"/).waitFor();assert.equal(await d.getByRole('button',{name:'Next',exact:true}).isDisabled(),true);
    await d.getByRole('button',{name:'Previous',exact:true}).click();await d.getByText(/owner-V-item-21/).waitFor();await d.getByRole('button',{name:'Remove conversation item',exact:true}).first().click();d=page.getByRole('dialog');await d.getByText('item-21',{exact:true}).waitFor();await confirm(d);
    assert.equal(state.items.get(`passthrough:${V}`).length,20);assert.equal(state.items.get(`passthrough:${U}`).length,21);
    d=await open(page,V,'Append conversation items');await d.locator('#resource-content-json').fill('[{"role":"assistant","content":"added-for-V"}]');await confirm(d);
    await page.locator('#managed-resource-mode').selectOption('node_dispatch');await rowFor(page,V).waitFor();d=await open(page,V,'Edit conversation metadata');assert.match(await d.locator('#resource-content-json').inputValue(),/node_dispatch-owner-V/);await d.getByRole('button',{name:'Cancel',exact:true}).click();
    state.conflict=true;d=await open(page,V,'Delete resource');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();await d.getByRole('button',{name:'Cancel',exact:true}).click();state.conflict=false;
    state.unavailable=true;const before=state.calls.filter(c=>c.method==='PATCH').length;d=await open(page,V,'Edit conversation metadata');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();await page.waitForTimeout(100);assert.equal(state.calls.filter(c=>c.method==='PATCH').length,before+1);await d.getByRole('button',{name:'Cancel',exact:true}).click();state.unavailable=false;
    d=await open(page,V,'Delete resource');await confirm(d);await rowFor(page,V).waitFor({state:'detached'});assert.equal(state.conversations.find(r=>r.mode==='node_dispatch'&&r.owner_user_id===U).deleted,false);
    await page.locator('#managed-resource-owner').fill('00000000-0000-0000-0000-000000000000');let count=state.calls.length;await page.getByRole('button',{name:'Apply owner filter',exact:true}).click();await page.locator('.tenant-response-admin > [role="alert"]').waitFor();assert.equal(state.calls.length,count);
    await page.locator('#managed-resource-owner').fill(U);await page.getByRole('button',{name:'Apply owner filter',exact:true}).click();await rowFor(page,U).waitFor();
    assert.equal(new URLSearchParams(state.calls.filter(c=>c.method==='GET'&&c.path.endsWith('/conversations')).at(-1).query).get('owner_user_id'),U);
    await page.screenshot({path:path.join(output,'resources.png'),fullPage:true});assert.deepEqual(state.unknown,[]);
    cases.push('full identity collisions, cursor paging, escaped private content, versioned metadata/items/cancel/delete, literal owner and single dispatch');await context.close();
    ({state,page,context}=await fixture());await page.goto(base+'/tenant/responses');await rowFor(page,V).waitFor();
    d=await open(page,V,'Inspect resource content');await d.getByText(/passthrough-owner-V-private/).waitFor();
    await page.setViewportSize({width:390,height:844});
    assert.match(await d.evaluate(el=>getComputedStyle(el).backgroundColor),/^rgb\(/,'private content needs an opaque theme surface');
    assert.equal(await d.evaluate(el=>getComputedStyle(el).opacity),'1','private content must not fade through the underlying page');
    assert.equal(await d.evaluate(el=>getComputedStyle(el).animationName),'none','private dialog must not inherit page opacity animation');
    const bounds=await d.boundingBox();assert.ok(bounds&&bounds.x>=0&&bounds.x+bounds.width<=391,'private inspector must remain within a narrow viewport');
    await page.screenshot({path:path.join(output,'resources-mobile.png'),fullPage:true});
    assert.equal(await d.evaluate(el=>el.contains(document.activeElement)),true,'dialog must receive keyboard focus on mount');
    await page.keyboard.press('Escape');await d.waitFor({state:'detached'});
    await page.getByRole('button',{name:'Conversations',exact:true}).click();await rowFor(page,V).waitFor();
    d=await open(page,V,'Edit conversation metadata');
    const editorBounds=await d.boundingBox();assert.ok(editorBounds&&editorBounds.x>=0&&editorBounds.x+editorBounds.width<=391,'editor must remain within a narrow viewport');
    assert.equal(await d.evaluate(el=>el.contains(document.activeElement)),true,'dialog must receive keyboard focus on mount');
    await page.keyboard.press('Escape');await d.waitFor({state:'detached'});assert.deepEqual(state.unknown,[]);
    cases.push('private inspectors and versioned editors remain usable on a narrow viewport');await context.close();
    ({state,page,context}=await fixture());state.hold=true;await page.goto(base+'/tenant/responses');await rowFor(page,U).waitFor();d=await open(page,U,'Request response cancellation');await d.getByRole('button',{name:'Confirm',exact:true}).click();
    for(let n=0;n<200&&!state.release;n++)await page.waitForTimeout(10);assert.ok(state.release);
    await d.press('Escape');assert.equal(await d.isVisible(),true,'pending mutation cannot be dismissed as cancelled');
    await page.evaluate(()=>{history.pushState(null,'','/tenant');window.dispatchEvent(new PopStateEvent('popstate'));});await page.locator('#workspace-select').waitFor();await page.locator('#workspace-select').selectOption(B);await page.getByRole('button',{name:'Switch workspace',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant B');
    state.release();await page.waitForTimeout(150);assert.doesNotMatch(await page.locator('body').innerText(),/Resource operation returned|same-resource-id/);
    await page.goto(base+'/tenant/responses');await page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(state.calls.filter(c=>c.method==='POST'&&c.path.endsWith('/cancel')).length,1);assert.equal(state.calls.filter(c=>c.path.includes(`${B}/responses`)).length,0);assert.deepEqual(state.unknown,[]);
    cases.push('pending original-owner command cannot replay or publish after workspace switch');await context.close();
    for(const config of [{selected:null,role:'root'},{selected:null,role:'operator'},{admin:false}]){
        ({state,page,context}=await fixture(config));await page.goto(base+'/tenant/responses');await page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(state.calls.filter(c=>/\/(responses|conversations)/.test(c.path)).length,0);assert.deepEqual(state.unknown,[]);await context.close();
    }
    ({state,page,context}=await fixture());state.foreign=true;await page.goto(base+'/tenant/responses');await page.locator('.tenant-response-admin [role="alert"]').waitFor();assert.equal(await page.locator('tbody tr').count(),0);assert.equal(await page.getByRole('button',{name:'Inspect resource content',exact:true}).count(),0);await context.close();
    cases.push('global labels and ordinary membership never grant tenant management; foreign rows fail closed');
    assert.deepEqual(errors,[]);
    const report={passed:true,basis:'production-compiled WASM with synthetic HTTP; backend authority is tested separately',cases,page_errors:errors};await fs.writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));
}catch(error){if(currentPage&&!currentPage.isClosed()){console.error((await currentPage.locator('body').innerText()).slice(0,9000));await currentPage.screenshot({path:path.join(output,'failure.png'),fullPage:true}).catch(()=>{});}console.error(error);process.exitCode=1;
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
