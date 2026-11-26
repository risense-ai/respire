import assert from 'node:assert/strict';
import {once} from 'node:events';
import {readFile} from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {server} from './ref/prototypes/mock-api/server.mjs';
import {chromium} from '@playwright/test';
import {quickPanelAdapter,verifyWebUIBundle} from './ref/test-cases/client2-e2e/compiled-webui/quick-panel-adapter.mjs';
import {verifyWailsRuntime} from './ref/test-cases/client2-e2e/compiled-webui/workspace-mock.mjs';

const [head, digest] = process.argv.slice(2);
const assets = await verifyWebUIBundle('/input/bundle',head,digest);
const wails = await verifyWailsRuntime('/app/wails.js');
const captions = JSON.parse(await readFile('/app/ref/test-cases/fixtures/i18n/compiled-workspace.json'));
const mock=server();
await once(mock.listen(0,'127.0.0.1'),'listening');
const response=await fetch('http://127.0.0.1:'+mock.address().port+'/v1/status/light',
  {headers:{authorization:'Bearer synthetic-preview-only'}});
assert.equal(response.status,200);
const expectedCount=(await response.json()).memory_count;
assert(Number.isInteger(expectedCount)&&expectedCount>0);
const browser = await chromium.launch({headless:true});
try {
  for (const locale of ['en','zh']) {
    const {app,count} = quickPanelAdapter(assets,{locale,surface:'overview',wails});
    assert.equal(count,expectedCount);
    const context = await browser.newContext({locale,serviceWorkers:'block',viewport:{width:1024,height:768},
      recordVideo:{dir:'/tmp',size:{width:1024,height:768}}});
    try {
      await once(app.listen(0,'127.0.0.1'),'listening');
      const origin = 'http://127.0.0.1:'+app.address().port;
      await context.route('**/*',route=>new URL(route.request().url()).origin===origin ? route.continue() : route.abort());
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror',()=>errors.push('page error'));
      await page.goto(origin+'/workspace.html#overview');
      await page.waitForFunction(({title,count})=>
        document.querySelector('#view .view-page .page-heading h1')?.textContent.trim()===title &&
        document.querySelector('[data-overview-summary] .metric strong')?.textContent===count,
        {title:captions.overview[locale],count:new Intl.NumberFormat(locale).format(count)});
      assert.equal(await page.locator('html').getAttribute('lang'),locale==='en'?'en':'zh-CN');
      await page.evaluate(()=>{
        const dot=document.createElement('div');
        dot.style.cssText='position:fixed;width:14px;height:14px;border:2px solid white;border-radius:50%;background:#156bd1;pointer-events:none;z-index:2147483647';
        document.body.append(dot);
        document.addEventListener('pointermove',e=>{dot.style.left=e.clientX+'px';dot.style.top=e.clientY+'px';});
      });
      await page.mouse.move(130,140);
      await page.mouse.move(500,280,{steps:35});
      await page.screenshot({path:'/out/'+locale+'.png',timeout:5000});
      await page.waitForTimeout(800);
      const video=page.video();
      await page.close();
      await context.close();
      execFileSync('ffmpeg',['-nostdin','-loglevel','error','-i',await video.path(),'-t','12','-an','-map_metadata','-1','-c:v','libx264','-pix_fmt','yuv420p','-movflags','+faststart','/out/'+locale+'.mp4'],{timeout:30000});
      assert.equal(errors.length,0);
    } finally {
      await context.close();
      app.closeAllConnections();
      await new Promise(resolve=>app.close(resolve));
    }
  }
} finally { await browser.close(); mock.closeAllConnections(); await new Promise(resolve=>mock.close(resolve)); }
