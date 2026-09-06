// Actual production app, real service worker/CacheStorage/EventSource and two
// deployments. The synthetic server never reads or changes a user's workspace.
import assert from "node:assert/strict";
import { createServer as createHttpServer } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, extname } from "node:path";
import { build, createServer as createViteServer } from "vite";
import { chromium, webkit } from "playwright";

const directory = await mkdtemp(join(tmpdir(), "lantor-shell-e2e-"));
const isWebKit = process.env.SHELL_BROWSER === "webkit";
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12,"0")}`;
const channelId = id(1), now = "2026-09-06T12:00:00Z";
const message = (n, body, extra = {}) => ({ id:id(n), seq:n, channel_id:channelId, thread_root_id:null,
  sender_agent_id:null, sender_name:"Owner", sender_role:"owner", body, is_task:false, thread_followed:false,
  delivery_state:"complete", stream_key:"", task_number:null, task_status:null, attachments:[], artifacts:[], created_at:now, updated_at:now, ...extra });
const state = {
  db_url:"synthetic://shell", web_base_url:null, owner_profile:{display_name:"Owner",avatar:"O",description:""},
  channels:[{id:channelId,name:"shell-test",description:"",kind:"channel",dm_agent_id:null,unread_count:0,github_unread_count:0,github_review_synced_at:null}],
  messages:[message(2,"Online workspace data")], channel_message_history:[{channel_id:channelId,before_seq:null,has_more:false}],
  thread_activities:[], channel_members:[], agents:[], saved_messages:[], dismissed_inbox_items:{}, read_inbox_items:{}, artifacts:[], tasks:[], reminders:[], agent_schedules:[], agent_runs:[], agent_work_items:[], agent_activities:[],
  supervisor:{pid:null,status:"stopped",updated_at:null}, launch_agent:{label:"",plist_path:"",installed:false,loaded:false}, ui_event_cursor:0,
};
const clients = new Set(), events = [], requests = [];
const sse = (entry) => `id: ${entry.cursor}\nevent: lantor\ndata: ${entry.event}\n\n`;
function publish(event) {
  const entry = {cursor:events.length+1,event:JSON.stringify(event)};
  events.push(entry); for (const client of clients) client.write(sse(entry));
}
let deployment = "a", brokenAsset = null, releaseAsset, waitingAsset, networkOffline = false;
const mime = {".js":"application/javascript", ".css":"text/css", ".html":"text/html", ".png":"image/png", ".woff":"font/woff", ".woff2":"font/woff2", ".ttf":"font/ttf", ".webmanifest":"application/manifest+json"};
const api = createHttpServer(async (request,response) => {
  const url = new URL(request.url,"http://localhost");
  requests.push({path:url.pathname,method:request.method});
  if (networkOffline) { request.socket.destroy(); return; }
  if (url.pathname === "/api/events") {
    response.writeHead(200,{"content-type":"text/event-stream","cache-control":"no-cache"}); response.write(": ready\n\n");
    for (const entry of events) if (entry.cursor > Number(url.searchParams.get("cursor"))) response.write(sse(entry));
    clients.add(response); response.on("close",()=>clients.delete(response)); return;
  }
  if (url.pathname.startsWith("/attachments/")) { response.writeHead(200,{"content-type":"text/plain"}).end("private attachment"); return; }
  if (url.pathname.startsWith("/api/")) {
    let raw=""; for await (const chunk of request) raw+=chunk;
    const args=raw ? JSON.parse(raw) : {};
    let result={ok:true};
    switch(url.pathname) {
      case "/api/bootstrap": result={...state,ui_event_cursor:events.length}; break;
      case "/api/load_channel_previews": case "/api/load_activity_messages": result=[]; break;
      case "/api/load_channel_messages": result={messages:state.messages,next_before_seq:null,has_more:false}; break;
      case "/api/load_ui_state": result=Object.fromEntries(args.scopes.map((s)=>[s,state[s]])); break;
      case "/api/load_message": result=state.messages.find((m)=>m.id===args.messageId); break;
      case "/api/replay_ui_events": result={cursor:events.length,replayGap:false,events:events.filter((e)=>e.cursor>args.cursor)}; break;
    }
    response.writeHead(200,{"content-type":"application/json","cache-control":"no-store"}).end(JSON.stringify(result)); return;
  }
  const root=join(directory,deployment);
  if (url.pathname === brokenAsset) await waitingAsset;
  const file=resolve(root,url.pathname === "/" || url.pathname === brokenAsset ? "index.html" : url.pathname.slice(1));
  if (!file.startsWith(root+"/")) { response.writeHead(404).end(); return; }
  try { response.writeHead(200,{"content-type":mime[extname(file)]||"application/octet-stream","cache-control":url.pathname.startsWith("/assets/") ? "public,max-age=31536000,immutable" : "no-cache"}).end(await readFile(file)); }
  catch { response.writeHead(404).end(); }
});
let browser, dev;
try {
  const manifests={};
  for (const version of ["a","b"]) {
    await build({logLevel:"silent",build:{outDir:join(directory,version)},plugins:[{
      name:"shell-build-probe",enforce:"pre",transform(source,path) {
        if (path.endsWith("/src/main.tsx")) return source+`\nwindow.__shellFixtureBuild = ${JSON.stringify(version)};`;
      },
    }]});
    const worker=await readFile(join(directory,version,"sw.js"),"utf8");
    manifests[version]=JSON.parse(worker.match(/const \{ version, entries \} = (.*);/)[1]);
  }
  await new Promise((done)=>api.listen(0,"127.0.0.1",done));
  const origin=`http://127.0.0.1:${api.address().port}`;
  browser=await (isWebKit ? webkit : chromium).launch({headless:true});
  const context=await browser.newContext({viewport:{width:1200,height:900}});
  const page=await context.newPage(); page.setDefaultTimeout(15000);
  const errors=[]; page.on("pageerror",(e)=>errors.push(e.message));
  await page.goto(origin);
  await page.getByText("Online workspace data",{exact:true}).waitFor();
  await page.waitForFunction(()=>navigator.serviceWorker.controller);
  await page.evaluate(()=>navigator.serviceWorker.ready);
  const cacheState=()=>page.evaluate(async()=>{
    const result={};
    for (const name of (await caches.keys()).filter((n)=>n.startsWith("lantor-shell-v1-"))) result[name]=(await (await caches.open(name)).keys()).map((r)=>new URL(r.url).pathname).sort();
    return result;
  });
  const initial=await cacheState();
  assert.equal(Object.keys(initial).length,1);
  assert.deepEqual(Object.values(initial)[0],manifests.a.entries.map((e)=>e.url).sort());
  assert.equal(await page.locator(".web-app-status").count(),0,"first install must not announce an update");
  assert.equal(await page.evaluate(async()=>(await navigator.serviceWorker.getRegistration()).updateViaCache),"none");
  const asset=manifests.a.entries.find((e)=>e.url.endsWith(".woff2")).url;
  const assetResponse=page.waitForResponse((r)=>new URL(r.url()).pathname===asset);
  await page.evaluate((path)=>fetch(path).then((r)=>r.arrayBuffer()),asset);
  assert.equal((await assetResponse).fromServiceWorker(),true);

  for (const [path,method] of [["/api/probe","GET"],["/api/mutate_probe","POST"],["/attachments/probe.txt","GET"]]) {
    const response=page.waitForResponse((r)=>new URL(r.url()).pathname===path);
    await page.evaluate(([url,method])=>fetch(url,{method}).then((r)=>r.text()),[path,method]);
    assert.equal((await response).fromServiceWorker(),false,`${path} bypasses worker fetch handling`);
  }
  const eventsResponse=page.waitForResponse((r)=>new URL(r.url()).searchParams.has("probe"));
  await page.evaluate(()=>new Promise((done)=>{window.__shellSseProbe=new EventSource("/api/events?probe=1");window.__shellSseProbe.onopen=done;}));
  assert.equal((await eventsResponse).fromServiceWorker(),false,"SSE bypasses worker fetch handling");
  await page.evaluate(()=>window.__shellSseProbe.close());
  const privatePage=await context.newPage();
  assert.equal((await privatePage.goto(origin+"/api/bootstrap")).fromServiceWorker(),false,"API navigation is not replaced with index.html");
  await privatePage.close();
  assert.deepEqual(await cacheState(),initial,"API/SSE/attachments never enter CacheStorage");

  const draft=page.getByPlaceholder("Message #shell-test");
  await draft.fill("Keep my unsent draft");
  // WebKit's automation offline mode fails navigation even for a worker that
  // only returns a constant Response. Sever the actual server connections
  // instead, supplying just the OS online/offline signal to that engine's UI.
  const connectionSignal=(online)=>{
    Object.defineProperty(navigator,"onLine",{configurable:true,get:()=>online});
    window.dispatchEvent(new Event(online ? "online" : "offline"));
  };
  async function setOffline(value) {
    if (!isWebKit) { await context.setOffline(value); return; }
    networkOffline=value;
    if (value) { for (const client of clients) client.destroy(); api.closeAllConnections(); }
    for (const openPage of context.pages()) await openPage.evaluate(connectionSignal,!value);
  }
  await setOffline(true);
  const offline=await context.newPage();
  if (isWebKit) await offline.addInitScript(connectionSignal,false);
  assert.equal((await offline.goto(origin+"/?offline-open=1")).fromServiceWorker(),true);
  await offline.locator(".boot.offline").waitFor();
  await offline.getByText("Offline — reconnect to sync your workspace.",{exact:true}).waitFor();
  assert.equal(await offline.getByText("Online workspace data",{exact:true}).count(),0,"offline shell does not cache workspace content");
  await offline.setViewportSize({width:390,height:844});
  if (process.env.SHELL_SCREENSHOT) await offline.screenshot({path:process.env.SHELL_SCREENSHOT+"-offline.png"});
  await offline.reload();
  await offline.locator(".boot.offline").waitFor();
  await setOffline(false);
  await offline.getByText("Online workspace data",{exact:true}).waitFor();
  await offline.close();
  const recovered=message(3,"SSE recovered after offline"); state.messages.push(recovered); publish({type:"message_upsert",message:recovered});
  await page.getByText(recovered.body,{exact:true}).waitFor();
  assert.equal(await draft.inputValue(),"Keep my unsent draft");

  // A partial deployment returns HTML instead of a JS chunk. Integrity must
  // reject installation, keep A controlling, and remove the incomplete B cache.
  deployment="b";
  brokenAsset=manifests.b.entries.find((e)=>e.url.endsWith(".js")&&!manifests.a.entries.some((a)=>a.url===e.url)).url;
  waitingAsset=new Promise((done)=>{releaseAsset=done;});
  const rejected=page.evaluate(async()=>{
    const reg=await navigator.serviceWorker.getRegistration();
    const result=new Promise((done)=>reg.addEventListener("updatefound",()=>{
      const worker=reg.installing;
      worker.addEventListener("statechange",()=>{if (["activated","redundant"].includes(worker.state)) done(worker.state);});
    },{once:true}));
    await reg.update(); return result;
  });
  await page.waitForFunction(async()=> (await caches.keys()).filter((n)=>n.startsWith("lantor-shell-v1-")).length===2);
  await page.evaluate(()=>navigator.serviceWorker.controller.postMessage({type:"LANTOR_SHELL_READY"}));
  await page.waitForTimeout(1700);
  assert.equal(Object.keys(await cacheState()).length,2,"old worker cleanup must not delete an installing cache");
  releaseAsset();
  assert.equal(await rejected,"redundant");
  await page.waitForFunction(async()=> (await caches.keys()).filter((n)=>n.startsWith("lantor-shell-v1-")).length===1);
  assert.equal(await page.locator(".web-app-status").count(),0);

  const stream=message(4,"Streaming before deploy",{sender_role:"agent",delivery_state:"streaming",stream_key:`${id(40)}:response`});
  state.messages.push(stream); publish({type:"message_upsert",message:stream});
  await page.getByText(stream.body,{exact:true}).waitFor();
  brokenAsset=null;
  await page.evaluate(async()=> (await navigator.serviceWorker.getRegistration()).update());
  await page.getByText("A new version is available.",{exact:true}).waitFor();
  assert.equal(await page.evaluate(()=>window.__shellFixtureBuild),"a","activation never reloads an open page");
  assert.equal(await draft.inputValue(),"Keep my unsent draft");
  stream.body+=" — still streaming after deploy";
  publish({type:"message_delta",message_id:stream.id,append:" — still streaming after deploy",body_length:Array.from(stream.body).length,delivery_state:"streaming"});
  await page.getByText(stream.body,{exact:true}).waitFor();
  assert.equal(Object.keys(await cacheState()).length,2,"live A tab keeps its asset version");
  if (process.env.SHELL_SCREENSHOT) await page.screenshot({path:process.env.SHELL_SCREENSHOT+"-update.png"});
  const oldAsset=manifests.a.entries.find((e)=>e.url.endsWith(".js")&&!manifests.b.entries.some((b)=>b.url===e.url)).url;
  const oldResponse=page.waitForResponse((r)=>new URL(r.url()).pathname===oldAsset);
  await page.evaluate((path)=>fetch(path).then((r)=>{if (!r.ok) throw Error("old chunk missing"); return r.text();}),oldAsset);
  assert.equal((await oldResponse).fromServiceWorker(),true);
  const math=message(5,"New lazy math after deploy: $$x^2$$"); state.messages.push(math); publish({type:"message_upsert",message:math});
  await page.locator(".katex").waitFor();
  await page.getByRole("button",{name:"Refresh",exact:true}).click();
  await page.waitForFunction(()=>window.__shellFixtureBuild==="b");
  await page.getByText(recovered.body,{exact:true}).waitFor();
  assert.equal(await page.locator(".web-app-status").count(),0);
  await page.waitForFunction(async()=> (await caches.keys()).filter((n)=>n.startsWith("lantor-shell-v1-")).length===1);
  const final=await cacheState();
  assert.deepEqual(Object.values(final)[0],manifests.b.entries.map((e)=>e.url).sort());
  assert.deepEqual(errors,[]);

  // Same production assets, desktop runtime flag: no registration at all.
  const desktop=await browser.newContext();
  await desktop.addInitScript(()=>{window.__TAURI_INTERNALS__={invoke:()=>Promise.reject(Error("fixture desktop backend unavailable"))};});
  const desktopPage=await desktop.newPage(); await desktopPage.goto(origin); await desktopPage.waitForTimeout(250);
  assert.equal(await desktopPage.evaluate(async()=> (await navigator.serviceWorker.getRegistrations()).length),0);
  await desktop.close();
  // Vite dev, even with build metadata present, never registers a worker.
  dev=await createViteServer({logLevel:"silent",server:{host:"127.0.0.1",strictPort:false,proxy:{"/api":{target:origin}}},plugins:[{name:"dev-shell-probe",transformIndexHtml:()=>[{tag:"meta",attrs:{name:"lantor-shell-version",content:"dev-fixture"},injectTo:"head"}]}]});
  await dev.listen();
  const devContext=await browser.newContext(), devPage=await devContext.newPage();
  await devPage.goto(`http://127.0.0.1:${dev.httpServer.address().port}`);
  await devPage.getByText(recovered.body,{exact:true}).waitFor();
  assert.equal(await devPage.evaluate(async()=> (await navigator.serviceWorker.getRegistrations()).length),0);
  await devContext.close();
  console.log(JSON.stringify({browser:isWebKit ? "webkit" : "chromium",precacheEntries:manifests.b.entries.length,versionA:manifests.a.version,versionB:manifests.b.version,checks:"cache allowlist, API/SSE/attachment bypass, offline open+reload, cold online recovery, SSE recovery, failed install rollback, concurrent install/cleanup, update prompt, preserved draft/stream/old lazy chunks, explicit refresh, old cache cleanup, Tauri/dev exclusion"}));
} finally {
  releaseAsset?.();
  await browser?.close(); await dev?.close();
  for (const client of clients) client.end();
  api.closeAllConnections(); await new Promise((done)=>api.close(done));
  await rm(directory,{recursive:true,force:true});
}
