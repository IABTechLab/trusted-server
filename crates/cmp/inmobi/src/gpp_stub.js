(function(cmpId){
if(typeof window.__gpp==="function")return;
var apis=["2:tcfeuv2","6:uspv1","7:usnatv1","8:usca","9:usvav1","10:uscov1","11:usutv1","12:usctv1"];
function ping(){return{gppVersion:"1.1",cmpStatus:"stub",cmpDisplayStatus:"hidden",signalStatus:"not ready",supportedAPIs:apis,cmpId:cmpId,sectionList:[],applicableSections:[-1],gppString:"",parsedSections:{}};}
var stub=function(){var b=arguments;stub.queue=stub.queue||[];stub.events=stub.events||[];
if(!b.length||(b.length==1&&b[0]=="queue"))return stub.queue;
if(b.length==1&&b[0]=="events")return stub.events;
var cmd=b[0],clb=b.length>1?b[1]:null,par=b.length>2?b[2]:null;
function answer(v,ok){if(typeof clb==="function")clb(v,ok);}
if(cmd==="ping"){answer(ping(),true);}
else if(cmd==="addEventListener"){if(!("lastId" in stub))stub.lastId=0;stub.lastId++;var lnr=stub.lastId;
stub.events.push({id:lnr,callback:clb,parameter:par});
answer({eventName:"listenerRegistered",listenerId:lnr,data:true,pingData:ping()},true);}
else if(cmd==="removeEventListener"){var ok=false;
for(var i=0;i<stub.events.length;i++){if(stub.events[i].id==par){stub.events.splice(i,1);ok=true;break;}}
answer({eventName:"listenerRemoved",listenerId:par,data:ok,pingData:ping()},true);}
else if(cmd==="hasSection"){answer(false,true);}
else if(cmd==="getSection"||cmd==="getField"){answer(null,true);}
else{stub.queue.push([].slice.apply(b));}};
window.__gpp_addFrame=function(n){if(!window.frames[n]){if(document.body){var f=document.createElement("iframe");
f.style.cssText="display:none";f.name=n;document.body.appendChild(f);}else{setTimeout(window.__gpp_addFrame,10,n);}}};
window.__gpp_msghandler=function(e){var s=typeof e.data==="string",d=null;try{d=s?JSON.parse(e.data):e.data;}catch(x){}
var c=d&&typeof d==="object"?d.__gppCall:null;if(c&&typeof c==="object"){window.__gpp(c.command,function(r,ok){
var m={__gppReturn:{returnValue:r,success:ok,callId:c.callId}};if(e.source)e.source.postMessage(s?JSON.stringify(m):m,"*");},
"parameter" in c?c.parameter:null,"version" in c?c.version:"1.1");}};
window.__gpp_stub=stub;window.__gpp=stub;
window.addEventListener("message",window.__gpp_msghandler,false);
window.__gpp_addFrame("__gppLocator");
})
