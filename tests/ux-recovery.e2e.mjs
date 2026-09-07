// Production app with synthetic responses only; never starts/stops a real agent.
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build, preview } from "vite";
import { chromium } from "playwright";
const directory = await mkdtemp(join(tmpdir(), "lantor-ux-recovery-"));
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = "2026-09-07T00:00:00Z", channelId = id(1), rootId = id(2);
const agent = (n, name) => ({ id:id(n), handle:name, display_name:name, role:"agent", status:"idle", runtime:"test", model:"", reasoning_effort:"", service_tier:"", avatar:name[0], description:"", launch_command:"", environment_variables:"", working_directory:"", workspace_exists:false, workspace_memory_path:"", workspace_memory_exists:false, workspace_entries:[], daily_budget_micros:0, subscription_status:null });
const message = (n, body, extra={}) => ({ id:id(n), seq:n, channel_id:channelId, thread_root_id:null, sender_agent_id:null, sender_name:"Owner", sender_role:"owner", body, is_task:false, thread_followed:false, delivery_state:"complete", stream_key:"", task_number:null, task_status:null, attachments:[], artifacts:[], created_at:now, updated_at:now, ...extra });
const work = (n, agentId, status, extra={}) => ({ id:id(n), agent_id:id(agentId), agent_handle:agentId===10 ? "Alpha" : "Beta", channel_id:channelId, channel_name:"recovery-test", thread_root_id:null, source_message_id:rootId, task_id:null, task_number:null, source_kind:"mention", title:`Request ${n}`, context:"", status, run_id:null, retry_work_item_id:null, failure_detail:'{"error":{"message":"Provider connection closed"}}', created_at:now, updated_at:now, completed_at:status === "failed" ? now : null, ...extra });
const state = {
  db_url:"synthetic://recovery", web_base_url:null, owner_profile:{display_name:"Owner",avatar:"O",description:""},
  channels:[{id:channelId,name:"recovery-test",description:"",kind:"channel",dm_agent_id:null,unread_count:0,github_unread_count:0,github_review_synced_at:null}],
  messages:[message(2,"Original thread @Alpha"), message(3,"Existing reply",{thread_root_id:rootId})],
  channel_message_history:[{channel_id:channelId,before_seq:null,has_more:false}],
  thread_activities:[], channel_members:[{channel_id:channelId,agent_id:id(10)},{channel_id:channelId,agent_id:id(11)}],
  agents:[agent(10,"Alpha"),agent(11,"Beta")], saved_messages:[], dismissed_inbox_items:{}, read_inbox_items:{}, artifacts:[], tasks:[], reminders:[], agent_schedules:[],
  agent_runs:[{id:id(31),agent_id:id(11),agent_handle:"Beta",work_item_id:id(21),command:"",working_directory:"",status:"running",pid:null,exit_code:null,log:"",input_tokens:0,output_tokens:0,cost_micros:0,started_at:now,stopped_at:null}],
  agent_work_items:[work(20,10,"failed"),work(21,11,"running",{run_id:id(31)}),work(22,10,"failed",{thread_root_id:rootId})],
  agent_activities:[], supervisor:{pid:null,status:"stopped",updated_at:null}, launch_agent:{label:"",plist_path:"",installed:false,loaded:false}, ui_event_cursor:0,
};
let server, browser;
const pendingSends = [], pendingActions = [], requests = [], errors = [];
try {
  await build({ logLevel:"silent", build:{outDir:directory} });
  server = await preview({ build:{outDir:directory}, preview:{host:"127.0.0.1",port:0,strictPort:true} });
  browser = await chromium.launch({headless:true});
  const page = await browser.newPage({viewport:{width:1600,height:1000}, serviceWorkers:"block"});
  page.setDefaultTimeout(10000);
  page.on("pageerror", (error)=>errors.push(error.message));
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const args = route.request().headers()["content-type"]?.includes("application/json") ? route.request().postDataJSON() ?? {} : {};
    requests.push({path,args});
    if (path === "/api/events") return route.fulfill({contentType:"text/event-stream",body:": ready\n\n"});
    if (path === "/api/send_message") { pendingSends.push(route); return; }
    if (path === "/api/retry_agent_work" || path === "/api/cancel_agent_work") { pendingActions.push({path,args,route}); return; }
    let result = {};
    if (path === "/api/bootstrap") result=state;
    else if (path === "/api/load_ui_state") result=Object.fromEntries(args.scopes.map((scope)=>[scope,state[scope]]));
    else if (["/api/load_channel_previews","/api/load_activity_messages"].includes(path)) result=[];
    else if (["/api/load_channel_messages","/api/load_thread_messages"].includes(path)) result={messages:state.messages,next_before_seq:null,has_more:false};
    else if (path === "/api/load_message") result=state.messages.find((m)=>m.id===args.messageId);
    else if (path === "/api/load_thread") result=state.messages;
    else if (path === "/api/load_agent_detail") result={agent:state.agents.find((a)=>a.id===args.agentId),agent_runs:state.agent_runs,agent_activities:[],agent_work_items:state.agent_work_items};
    else if (path === "/api/replay_ui_events") result={cursor:0,replayGap:false,events:[]};
    return route.fulfill({json:result});
  });
  const origin=`http://127.0.0.1:${server.httpServer.address().port}`;
  await page.goto(origin);
  await page.getByPlaceholder("Message #recovery-test").waitFor();
  const conversation=page.locator(".conversation");
  await conversation.locator(".work-item-failure").filter({hasText:"Provider connection closed"}).waitFor();
  await page.reload();
  await conversation.locator(".work-item-failure").filter({hasText:"Provider connection closed"}).waitFor();
  await conversation.locator('a[href="/lantor/agent/Alpha"]').first().click();
  const drawer=page.locator(".agent-drawer");
  await drawer.getByRole("button",{name:"Retry request: Request 22",exact:true}).waitFor();
  await drawer.getByRole("button",{name:"Request 22",exact:true}).click();
  await page.locator(".thread").getByText("Provider connection closed",{exact:true}).waitFor();
  await page.getByRole("button",{name:"Retry request: Request 20",exact:true}).click();
  await page.getByRole("button",{name:"Retrying… request: Request 20",exact:true}).waitFor();
  assert.equal(pendingActions.length,1);
  assert.equal(await page.getByRole("button",{name:"Retrying… request: Request 20",exact:true}).isDisabled(),true);
  // Lost response: the server accepted this retry. The refresh must reveal its
  // existing attempt without an automatic second retry request.
  state.agent_work_items[0].retry_work_item_id=id(23);
  state.agent_work_items.unshift(work(23,10,"queued"));
  await pendingActions.shift().route.abort("failed");
  await page.getByRole("button",{name:"Stop request: Request 23",exact:true}).waitFor();
  assert.equal(requests.filter((r)=>r.path === "/api/retry_agent_work").length,1);
  await page.getByRole("button",{name:"Stop request: Request 21",exact:true}).click();
  assert.equal(pendingActions[0].args.workItemId,id(21));
  state.agent_work_items.find((w)=>w.id===id(21)).status="cancelling";
  await pendingActions.shift().route.fulfill({json:null});
  await page.getByRole("button",{name:"Stopping… request: Request 21",exact:true}).waitFor();
  await page.getByText("Beta is stopping",{exact:true}).waitFor();
  assert.equal(state.agent_work_items.find((w)=>w.id===id(23)).status,"queued");

  const root=page.getByPlaceholder("Message #recovery-test");
  await root.fill("Original root draft A");
  await conversation.locator('input[type="file"]').setInputFiles({name:"root-A.txt",mimeType:"text/plain",buffer:Buffer.from("A")});
  await root.press("Enter");
  await root.fill("New root draft B");
  await conversation.locator('input[type="file"]').setInputFiles({name:"root-B.txt",mimeType:"text/plain",buffer:Buffer.from("B")});
  assert.equal(pendingSends.length,1);
  await pendingSends.shift().fulfill({status:503,json:{message:"Delayed root send failure"}});
  await conversation.getByText("Message could not be sent",{exact:true}).waitFor();
  assert.equal(await root.inputValue(),"New root draft B");
  await conversation.getByRole("button",{name:"Add to draft",exact:true}).click();
  assert.equal(await root.inputValue(),"New root draft B\n\nOriginal root draft A");
  await conversation.getByText("root-A.txt",{exact:true}).waitFor();
  await conversation.getByText("root-B.txt",{exact:true}).waitFor();

  // Opening the original thread exposes its persistent failure, with the same
  // recovery actions and a separately buffered reply composer.
  await conversation.getByRole("button",{name:"Request failed: Alpha. View thread",exact:true}).click();
  const thread=page.locator(".thread");
  const reply=thread.locator("textarea");
  await reply.waitFor();
  await thread.getByText("Provider connection closed",{exact:true}).waitFor();
  await reply.fill("Original reply A");
  await reply.press("Enter");
  await reply.fill("New reply B");
  await pendingSends.shift().fulfill({status:503,json:{message:"Delayed reply send failure"}});
  await thread.getByText("Message could not be sent",{exact:true}).waitFor();
  assert.equal(await reply.inputValue(),"New reply B");
  if (process.env.UX_SCREENSHOT) await page.screenshot({path:process.env.UX_SCREENSHOT.replace(".png", "-desktop.png"),fullPage:true});
  await page.setViewportSize({width:390,height:844});
  await page.getByRole("button",{name:"Dismiss notification",exact:true}).click();
  const failedBox=await thread.locator(".failed-message-drafts").boundingBox();
  assert.ok(failedBox && failedBox.width>300,"mobile recovery uses the composer width");
  await thread.getByRole("button",{name:"Add to draft",exact:true}).click();
  await page.waitForFunction(() => document.querySelector(".thread textarea")?.value === "New reply B\n\nOriginal reply A");
  assert.equal(await reply.inputValue(),"New reply B\n\nOriginal reply A");
  const retry=page.getByRole("button",{name:"Retry request: Request 22",exact:true});
  await retry.scrollIntoViewIfNeeded();
  const box=await retry.boundingBox();
  assert.ok(box && box.x>=0 && box.x+box.width<=390,"retry remains reachable on a phone");
  const progressBox=await thread.locator(".thread-progress-layer").boundingBox();
  const rootBox=await thread.locator('article[data-message-id="'+rootId+'"]').boundingBox();
  assert.ok(rootBox && progressBox && rootBox.y>=progressBox.y+progressBox.height,"progress does not cover the original request");
  if (process.env.UX_SCREENSHOT) {
    await page.locator(".app-toast.error").waitFor({state:"hidden"});
    await page.screenshot({path:process.env.UX_SCREENSHOT,fullPage:true});
  }
  assert.deepEqual(errors,[]);
  console.log("UX recovery passed: refresh persistence, lost-response retry, per-agent stop, root/reply delayed failures, attachment recovery, phone controls.");
} finally {
  for (const route of pendingSends) await route.abort().catch(()=>{});
  for (const {route} of pendingActions) await route.abort().catch(()=>{});
  await browser?.close();
  await new Promise((resolve)=>server?.httpServer.close(resolve) ?? resolve());
  await rm(directory,{recursive:true,force:true});
}
