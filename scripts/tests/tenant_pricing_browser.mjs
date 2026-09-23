// Real WASM interaction checks with synthetic same-origin HTTP only.
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.KC_PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(process.env.KC_WEB_ROOT || 'target/dx/web/release/web/public');
const output = path.resolve(process.env.KC_BROWSER_OUTPUT || 'tenant-pricing-browser-results');
await fs.mkdir(output,{recursive:true});
const A='11111111-1111-4111-8111-111111111111',B='22222222-2222-4222-8222-222222222222',U='33333333-3333-4333-8333-333333333333',P='44444444-4444-4444-8444-444444444444';
const stamp='2026-09-23T00:00:00Z';
const server=http.createServer(async(req,res)=>{try{
    const pathname=decodeURIComponent(new URL(req.url,'http://localhost').pathname);let file=path.resolve(root,'.'+pathname);
    if(file!==root&&!file.startsWith(root+path.sep))throw Error('invalid asset path');
    let data;try{data=await fs.readFile(file);}catch{file=path.join(root,'index.html');data=await fs.readFile(file);}
    const types={'.html':'text/html','.js':'text/javascript','.wasm':'application/wasm','.css':'text/css','.woff2':'font/woff2','.png':'image/png','.jpg':'image/jpeg','.svg':'image/svg+xml','.ico':'image/x-icon'};
    res.writeHead(200,{'Content-Type':types[path.extname(file)]||'application/octet-stream','Cache-Control':'no-store'});res.end(data);
}catch{res.writeHead(500);res.end();}});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({headless:true,args:['--no-sandbox']});
let currentPage;const errors=[];const cases=[];
const price=(tenant=A)=>({id:P,scope_type:'tenant',tenant_id:tenant,model_name:'fixture-model',billing_dimension:'provideraccount',currency:'CNY',input_price_per_1k:'0.0000000001',output_price_per_1k:'9999999999.9999999999',is_default:false,is_effective:true,effective_from:stamp,effective_until:null,created_at:stamp,version:7});
async function fixture({selected=A,role='none',tenantAdmin=true}={}){
    const state={selected,role,tenantAdmin,rows:[price()],calls:[],unknown:[],unavailable:false,conflict:false,hold:false,release:null,foreign:false};
    const context=await browser.newContext({viewport:{width:1440,height:1000}});
    await context.addInitScript(()=>{localStorage.setItem('keyc_access_token','fixture-token');localStorage.setItem('keyc_lang','en');});
    const page=await context.newPage();page.setDefaultTimeout(20000);currentPage=page;page.on('pageerror',e=>errors.push(e.message));
    const profile=()=>({id:U,user_id:U,email:'owner@fixture.invalid',name:'Pricing fixture',platform_role:state.role,status:'active',created_at:stamp,
        selected_tenant:state.selected?{id:state.selected,name:state.selected===A?'Tenant A':'Tenant B',slug:'fixture',tenant_role:state.selected===A&&state.tenantAdmin?'admin':'member',authz_version:1,membership_authz_version:1}:null,
        memberships:[{tenant_id:A,tenant_name:'Tenant A',tenant_role:state.tenantAdmin?'admin':'member',status:'active',authz_version:1},{tenant_id:B,tenant_name:'Tenant B',tenant_role:'member',status:'active',authz_version:1}],
        capabilities:{platform:state.role==='root'?['users:manage']:state.role==='operator'?['platform:health']:[],tenant:state.selected===A&&state.tenantAdmin?['tenant:manage']:[]}});
    await page.route('**/*',async route=>{
        const request=route.request(),url=new URL(request.url());if(url.origin!==base)return route.abort();
        if(!url.pathname.startsWith('/api/'))return route.continue();
        const p=url.pathname,m=request.method(),body=request.postDataJSON();state.calls.push({path:p,method:m,query:url.search,body});
        const answer=(value,status=200)=>route.fulfill({status,contentType:'application/json',headers:{'Cache-Control':'private, no-store'},body:JSON.stringify(value)});
        if(p==='/api/v1/settings/public')return answer({site_name:'KeyCompute',distribution_enabled:false});
        if(p==='/api/v1/me'&&m==='GET')return answer(profile());
        if(p==='/api/v1/me/tenant') {state.selected=body.tenant_id;return answer({...profile(),access_token:'fixture-selected',token_type:'Bearer',expires_in:3600});}
        if(p===`/api/v1/tenants/${state.selected}`)return answer({id:state.selected,owner_user_id:U,name:state.selected===A?'Tenant A':'Tenant B',slug:'fixture',description:null,status:'active',default_rpm_limit:60,default_tpm_limit:100000,authz_version:1,tenant_role:state.selected===A?'admin':'member',membership_authz_version:1});
        const b=`/api/v1/tenants/${A}/pricing`;
        if(p===b&&m==='GET') {const n=Number(url.searchParams.get('page')||1),search=url.searchParams.get('search');let rows=state.rows;
            if(search) rows=rows.filter(r=>r.model_name.includes(search));
            const total=rows.length;return answer({pricing:n===1?(state.foreign?[price(B)]:rows):[],total,page:n,page_size:20,total_pages:Math.ceil(total/20)});}
        if(p===b&&m==='POST') {if(state.unavailable)return answer({error:{message:'uncertain creation'}},503);
            assert.equal(body.tenant_id,undefined);assert.equal(body.scope_type,undefined);const r={...price(),...body,id:'55555555-5555-4555-8555-555555555555',version:1};state.rows.push(r);return answer(r);}
        if(p===`${b}/${P}`&&m==='PATCH') {if(state.conflict)return answer({error:{message:'version conflict; reload'}},409);
            if(state.hold)await new Promise(resolve=>{state.release=resolve;});
            const r=state.rows.find(r=>r.id===P);assert.equal(body.expected_version,r.version);Object.assign(r,body,{version:r.version+1});return answer(r);}
        if(p===`${b}/${P}/make-default`){const r=state.rows.find(r=>r.id===P);r.is_default=true;r.version++;return answer(r);}
        if(p===`${b}/${P}`&&m==='DELETE'){state.rows=state.rows.filter(r=>r.id!==P);return answer({success:true,pricing_id:P});}
        state.unknown.push(`${m} ${p}`);return answer({error:{message:'unexpected fixture endpoint'}},404);
    });
    return {state,page,context};
}
async function dialog(page){const d=page.getByRole('dialog');await d.waitFor();return d;}
try{
    let {state,page,context}=await fixture();
    await page.goto(base+'/tenant/pricing');await page.getByText('fixture-model',{exact:true}).waitFor();
    assert.match(await page.locator('tbody').innerText(),/9999999999\.9999999999/);
    await page.getByRole('button',{name:'Edit price',exact:true}).first().click();let d=await dialog(page);
    assert.equal(await d.locator('#pricing-input').inputValue(),'0.0000000001');assert.equal(await d.locator('#pricing-model').isDisabled(),true);
    await d.locator('#pricing-input').fill('0.00000000001');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();
    assert.equal(state.calls.filter(c=>c.method==='PATCH').length,0);
    await d.locator('#pricing-input').fill('0.0000000002');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.waitFor({state:'detached'});
    assert.equal(state.calls.find(c=>c.method==='PATCH').body.expected_version,7);
    await page.locator('tbody').getByRole('button',{name:'Make default',exact:true}).click();d=await dialog(page);await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.waitFor({state:'detached'});await page.locator('tbody').getByText('Default',{exact:true}).waitFor();
    await page.getByRole('button',{name:'Create tenant price',exact:true}).click();d=await dialog(page);
    await d.locator('#pricing-model').fill('new-model');await d.locator('#pricing-input').fill('1.2345678901');await d.locator('#pricing-output').fill('2');
    await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.waitFor({state:'detached'});await page.getByText('new-model',{exact:true}).waitFor();
    await page.locator('#tenant-pricing-search').fill('needle&tenant_id=foreign');await page.getByRole('button',{name:'Search models',exact:true}).click();
    await page.getByText('No records',{exact:true}).waitFor();const searched=state.calls.filter(c=>c.method==='GET'&&c.path.endsWith('/pricing')).at(-1);
    assert.equal(new URLSearchParams(searched.query).get('search'),'needle&tenant_id=foreign');assert.equal(new URLSearchParams(searched.query).has('tenant_id'),false);
    await page.locator('#tenant-pricing-search').fill('');await page.getByRole('button',{name:'Search models',exact:true}).click();await page.getByText('fixture-model',{exact:true}).waitFor();
    state.conflict=true;await page.locator('tbody tr').filter({hasText:'fixture-model'}).getByRole('button',{name:'Edit price',exact:true}).click();d=await dialog(page);await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();
    assert.equal(state.calls.filter(c=>c.method==='PATCH').length,2);await d.getByRole('button',{name:'Cancel',exact:true}).click();state.conflict=false;
    state.unavailable=true;await page.getByRole('button',{name:'Create tenant price',exact:true}).click();d=await dialog(page);await d.locator('#pricing-model').fill('uncertain-model');await d.locator('#pricing-input').fill('1');await d.locator('#pricing-output').fill('1');await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.locator('[role="alert"]').waitFor();assert.equal(state.calls.filter(c=>c.method==='POST'&&c.path.endsWith('/pricing')).length,2);await d.getByRole('button',{name:'Cancel',exact:true}).click();
    await page.locator('tbody tr').filter({hasText:'fixture-model'}).getByRole('button',{name:'Delete price',exact:true}).click();d=await dialog(page);await d.getByRole('button',{name:'Confirm',exact:true}).click();await d.waitFor({state:'detached'});await page.waitForFunction(()=>!document.querySelector('tbody')?.textContent.includes('fixture-model'));
    await page.screenshot({path:path.join(output,'pricing.png')});assert.deepEqual(state.unknown,[]);cases.push('exact prices, versioned edit, create/default/delete, literal search and no automatic command replay');await context.close();
    ({state,page,context}=await fixture());state.hold=true;await page.goto(base+'/tenant/pricing');await page.getByText('fixture-model',{exact:true}).waitFor();await page.getByRole('button',{name:'Edit price',exact:true}).click();d=await dialog(page);await d.locator('#pricing-input').fill('0.7777777777');await d.getByRole('button',{name:'Confirm',exact:true}).click();
    for(let i=0;i<100&&!state.release;i++)await new Promise(r=>setTimeout(r,10));assert.ok(state.release);
    // SPA route change unmounts the pending form; selection starts a new workspace epoch.
    await page.evaluate(()=>{history.pushState(null,'','/tenant');window.dispatchEvent(new PopStateEvent('popstate'));});await page.locator('#workspace-select').waitFor();await page.locator('#workspace-select').selectOption(B);await page.getByRole('button',{name:'Switch workspace',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#tenant-name')?.value==='Tenant B');
    state.release();await page.waitForTimeout(150);assert.doesNotMatch(await page.locator('body').innerText(),/0\.7777777777|Changes saved/);assert.equal(state.calls.filter(c=>c.method==='PATCH').length,1);
    await page.goto(base+'/tenant/pricing');await page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(state.calls.filter(c=>c.path.includes(`${B}/pricing`)).length,0);assert.deepEqual(state.unknown,[]);cases.push('pending A command cannot replay or publish into member workspace B');await context.close();
    for(const role of ['root','operator']){({state,page,context}=await fixture({selected:null,role}));await page.goto(base+'/tenant/pricing');await page.getByText('Current tenant administrator capability is required.',{exact:true}).waitFor();assert.equal(state.calls.filter(c=>c.path.includes('/pricing')).length,0);assert.deepEqual(state.unknown,[]);await context.close();}
    ({state,page,context}=await fixture());state.foreign=true;await page.goto(base+'/tenant/pricing');await page.locator('.tenant-pricing-admin [role="alert"]').waitFor();assert.doesNotMatch((await page.locator('tbody').allTextContents()).join(' '),/fixture-model/);assert.equal(await page.getByRole('button',{name:'Edit price',exact:true}).count(),0);await context.close();cases.push('global role labels never grant tenant access; foreign response rows fail closed');
    const report={passed:true,basis:'production compiled WASM with synthetic HTTP; no production/backend-security claim',cases,page_errors:errors};await fs.writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));assert.deepEqual(errors,[]);
}catch(error){if(currentPage&&!currentPage.isClosed()){console.error((await currentPage.locator('body').innerText()).slice(0,6000));await currentPage.screenshot({path:path.join(output,'failure.png')}).catch(()=>{});}console.error(error);process.exitCode=1;
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
