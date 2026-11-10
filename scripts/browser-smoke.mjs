// End-to-end viewer check using installed Edge/Chrome and Node 24's built-in CDP WebSocket.
// Usage: node scripts/browser-smoke.mjs http://127.0.0.1:PORT/ [screenshot.png]
import {spawn} from 'node:child_process';
import {existsSync} from 'node:fs';
import {mkdtemp, mkdir, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import net from 'node:net';

const url=process.argv[2];
if(!url||!/^http:\/\/127\.0\.0\.1:\d+\//.test(url))throw new Error('Supply a running local viewer URL: http://127.0.0.1:PORT/');
const browser=process.env.POTREE_BROWSER||[
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
].find(existsSync);
if(!browser)throw new Error('No Edge/Chrome found; set POTREE_BROWSER.');
const listener=net.createServer();
await new Promise(resolve=>listener.listen(0,'127.0.0.1',resolve));
const port=listener.address().port;await new Promise(resolve=>listener.close(resolve));
const profile=await mkdtemp(path.join(tmpdir(),'potree-browser-'));
const child=spawn(browser,[
  '--headless=new','--no-first-run','--no-default-browser-check','--disable-extensions',
  '--disable-background-networking','--enable-unsafe-swiftshader',
  '--window-size=1280,900',`--user-data-dir=${profile}`,
  '--remote-debugging-address=127.0.0.1',`--remote-debugging-port=${port}`,'about:blank',
],{windowsHide:true,stdio:'ignore'});
let socket;
const delay=milliseconds=>new Promise(resolve=>setTimeout(resolve,milliseconds));
try{
  let targets;
  for(let attempt=0;attempt<100;attempt++){
    try{targets=await(await fetch(`http://127.0.0.1:${port}/json`)).json();if(targets.some(target=>target.type==='page'))break;}catch{}
    if(child.exitCode!==null)throw new Error('Browser exited before CDP started: '+child.exitCode);
    await delay(200);
  }
  const target=targets?.find(target=>target.type==='page');if(!target)throw new Error('Browser CDP endpoint did not start.');
  socket=new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
  let nextId=0;const pending=new Map();const exceptions=[];
  socket.addEventListener('message',event=>{
    const message=JSON.parse(event.data);
    if(message.id){const call=pending.get(message.id);if(call){pending.delete(message.id);message.error?call.reject(new Error(JSON.stringify(message.error))):call.resolve(message.result);}}
    else if(message.method==='Runtime.exceptionThrown')exceptions.push(message.params.exceptionDetails.text+': '+(message.params.exceptionDetails.exception?.description||''));
  });
  const send=(method,params={})=>new Promise((resolve,reject)=>{const id=++nextId;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params}));});
  await send('Runtime.enable');await send('Page.enable');
  await send('Page.navigate',{url});
  let state;let readySince=0;
  for(let attempt=0;attempt<240;attempt++){
    const result=await send('Runtime.evaluate',{expression:`(()=>{
      const element=document.getElementById('project-config');
      if(!element)return {loading:true};
      const config=JSON.parse(element.textContent);const access=document.getElementById('access');
      if(config.password&&access&&access.style.display==='flex'){
        document.getElementById('password').value=config.password;
        document.getElementById('access-form').dispatchEvent(new Event('submit',{cancelable:true}));
      }
      const viewer=window.viewer;
      return {title:document.title,failures:window.viewerFailures||[],status:document.getElementById('status')?.textContent,
        clouds:viewer?.scene.pointclouds.map(cloud=>({name:cloud.name,rootLoaded:!!cloud.pcoGeometry?.root.loaded,
          rootPoints:cloud.pcoGeometry?.root.numPoints,visibleNodes:cloud.visibleNodes.length})),
        expectedClouds:config.clouds.length,
        camera:viewer?.scene.view.position.toArray()};
    })()`,returnByValue:true});
    state=result.result?.value;
    if(state?.failures?.length)throw new Error('Viewer failures: '+JSON.stringify(state));
    if(state?.clouds?.length===state?.expectedClouds&&state.clouds.every(cloud=>cloud.rootLoaded&&cloud.visibleNodes>0)&&state.status==='Ready'){
      if(!readySince)readySince=Date.now();
      // Root readiness precedes progressive detail requests and GPU drawing.
      if(Date.now()-readySince>=4000)break;
    }
    await delay(250);
  }
  if(!state?.clouds?.length||!state.clouds.every(cloud=>cloud.rootLoaded&&cloud.visibleNodes>0))throw new Error('Viewer did not render decoded clouds: '+JSON.stringify(state));
  if(exceptions.length)throw new Error('Browser exceptions: '+exceptions.join('\n'));
  if(process.argv[3]){
    const screenshot=await send('Page.captureScreenshot',{format:'png'});
    await mkdir(path.dirname(path.resolve(process.argv[3])),{recursive:true});
    await writeFile(process.argv[3],Buffer.from(screenshot.data,'base64'));
  }
  console.log(JSON.stringify(state,null,2));
  await send('Browser.close').catch(()=>{});
}finally{
  socket?.close();child.kill();
  // Only remove the unique temporary browser profile created above.
  if(path.dirname(path.resolve(profile))===path.resolve(tmpdir())&&path.basename(profile).startsWith('potree-browser-')){
    for(let attempt=0;attempt<10;attempt++){try{await rm(profile,{recursive:true,force:true});break;}catch{await delay(200);}}
  }
}
