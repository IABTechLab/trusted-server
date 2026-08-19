import type { ApsRendererV1 } from '../core/types';
import { validateApsRenderer } from '../core/contracts/aps_renderer';
import { APS_RENDERER_VALIDATOR_ES5_V1 } from '../core/contracts/generated/renderer_validator_document_v1';

export const MAX_APS_CONTAINER_DOCUMENT_BYTES = 65_536;
export const MAX_APS_INNER_DOCUMENT_BYTES = 65_536;
export const MAX_APS_CONTAINER_URL_BYTES = 196_663;
export const APS_DATA_URL_PREFIX = 'data:text/html;charset=utf-8,';
export const APS_PERMANENT_SANDBOX =
  'allow-forms allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-same-origin allow-scripts allow-top-navigation-by-user-activation';

const bootstrapNoncePattern = /^b1_[A-Za-z0-9_-]{22}$/;
const rendererNoncePattern = /^n1_[A-Za-z0-9_-]{22}$/;
const loopbackIpv4Pattern = /^127(?:\.\d{1,3}){3}$/;
const sentinelPattern = /__TS_[A-Z0-9_]+__/;
const encoder = new TextEncoder();

function scriptSources(
  trustedServerOrigin: string,
  creativeOrigin: string,
  scriptCreative: boolean
): string {
  return scriptCreative
    ? `'unsafe-inline' ${trustedServerOrigin} ${creativeOrigin}`
    : `'unsafe-inline' ${trustedServerOrigin}`;
}

function outerCsp(
  trustedServerOrigin: string,
  creativeOrigin: string,
  scriptCreative: boolean
): string {
  return `default-src 'none'; base-uri 'none'; object-src 'none'; script-src ${scriptSources(trustedServerOrigin, creativeOrigin, scriptCreative)}; connect-src https: ${trustedServerOrigin}; frame-src data: ${creativeOrigin}; img-src https: data: blob:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;`;
}

function innerCsp(
  trustedServerOrigin: string,
  creativeOrigin: string,
  scriptCreative: boolean
): string {
  return `default-src 'none'; base-uri 'none'; object-src 'none'; script-src ${scriptSources(trustedServerOrigin, creativeOrigin, scriptCreative)}; connect-src https: ${trustedServerOrigin}; frame-src ${creativeOrigin}; img-src https: data: blob:; media-src https: blob:; style-src 'unsafe-inline' https:; font-src https: data:; worker-src https: blob:; form-action https:;`;
}

export const APS_IFRAME_OUTER_CSP = (trustedServerOrigin: string, creativeOrigin: string): string =>
  outerCsp(trustedServerOrigin, creativeOrigin, false);
export const APS_IFRAME_INNER_CSP = (trustedServerOrigin: string, creativeOrigin: string): string =>
  innerCsp(trustedServerOrigin, creativeOrigin, false);
export const APS_SCRIPT_OUTER_CSP = (trustedServerOrigin: string, creativeOrigin: string): string =>
  outerCsp(trustedServerOrigin, creativeOrigin, true);
export const APS_SCRIPT_INNER_CSP = (trustedServerOrigin: string, creativeOrigin: string): string =>
  innerCsp(trustedServerOrigin, creativeOrigin, true);

export interface ApsDataDocumentInputV1 {
  readonly renderer: unknown;
  readonly publisherOrigin: string;
  readonly bootstrapNonce: string;
  readonly rendererNonce: string;
}

export interface ApsDataDocumentsV1 {
  readonly version: 1;
  readonly bootstrapNonce: string;
  readonly rendererNonce: string;
  readonly trustedServerOrigin: string;
  readonly creativeOrigin: string;
  readonly sandbox: string;
  readonly outerCsp: string;
  readonly innerCsp: string;
  readonly outerDocument: string;
  readonly innerDocument: string;
  readonly outerUrl: string;
  readonly innerUrl: string;
}

const INNER_TEMPLATE = `<!doctype html>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="__TS_INNER_CSP__">
<style>html,body{border:0;height:100%;margin:0;overflow:hidden;padding:0;width:100%}</style>
<body>
<script>
(function(){
'use strict';
__TS_RENDERER_VALIDATOR__
var EXPECTED_NONCE=__TS_RENDERER_NONCE_JSON__;
var EXPECTED_PUBLISHER_ORIGIN=__TS_PUBLISHER_ORIGIN_JSON__;
var EXPECTED_CREATIVE_ORIGIN=__TS_CREATIVE_ORIGIN_JSON__;
var EXPECTED_TAG_TYPE=__TS_TAG_TYPE_JSON__;
var RUNNER_URL=__TS_RUNNER_URL_JSON__;
var MAX_GLOBAL_MESSAGE_BYTES=4096;
var port=null;
var accepted=false;
var terminal=false;
var runnerLoaded=false;
var callbackOutcome=null;
function utf8Length(value){return(new TextEncoder()).encode(value).length;}
function exactPlain(value,keys){
 return !!value&&typeof value==='object'&&!Array.isArray(value)&&
  Object.getPrototypeOf(value)===Object.prototype&&apsExactRecord(value,keys);
}
function exactOrigin(value){
 if(typeof value!=='string'||utf8Length(value)>2048||/[\\x00-\\x20\\x7f'";]/.test(value))return false;
 try{
  var parsed=new URL(value);
  return (parsed.protocol==='https:'||parsed.protocol==='http:')&&parsed.hostname!==''&&
   parsed.username===''&&parsed.password===''&&parsed.origin===value&&
   parsed.pathname==='/'&&parsed.search===''&&parsed.hash==='';
 }catch(_error){return false;}
}
function rendererMatchesPolicy(renderer){
 try{
  return renderer.tagType===EXPECTED_TAG_TYPE&&
   (new URL(renderer.creativeUrl)).origin===EXPECTED_CREATIVE_ORIGIN;
 }catch(_error){return false;}
}
function send(message){
 if(!port)return;
 try{port.postMessage(message);}catch(_error){/* A closed terminal port has no fallback channel. */}
}
function close(){
 if(!port)return;
 try{port.close();}catch(_error){/* Port cleanup remains best-effort after settlement. */}
}
function fail(reason){
 if(terminal)return;
 terminal=true;
 send({message:'TS APS Render Failed',version:1,nonce:EXPECTED_NONCE,reason:reason});
 close();
}
function finishCallback(){
 if(terminal||!runnerLoaded||callbackOutcome===null)return;
 if(callbackOutcome==='rejected')return fail('runner_failed');
 terminal=true;
 send({message:'TS APS Render Completed',version:1,nonce:EXPECTED_NONCE});
 close();
}
function receiveEnvelope(event){
 if(accepted||terminal)return;
 var envelope=event&&event.data;
 if(!exactPlain(envelope,['nonce','publisherOrigin','renderer','version'])||
    envelope.version!==1||envelope.nonce!==EXPECTED_NONCE||
    envelope.publisherOrigin!==EXPECTED_PUBLISHER_ORIGIN||
    !exactOrigin(envelope.publisherOrigin)||
    classifyApsRendererV1(envelope.renderer,envelope.publisherOrigin)!=='accepted'||
    !rendererMatchesPolicy(envelope.renderer)){
  fail('descriptor_invalid');
  return;
 }
 accepted=true;
 port.onmessage=null;
 send({message:'TS APS Document Accepted',version:1,nonce:EXPECTED_NONCE});
 window._aps=window._aps instanceof Map?window._aps:new Map();
 var renderer=envelope.renderer;
 var account=window._aps.get(renderer.accountId);
 if(!account){
  account={queue:[],store:new Map([['listeners',new Map()]])};
  window._aps.set(renderer.accountId,account);
 }
 var renderPromise=new Promise(function(resolve,reject){
  account.queue.push(new CustomEvent('prebid/creative/render',{detail:{
   aaxResponse:renderer.aaxResponse,
   seatBidId:renderer.bidId,
   source:'internal',
   resolve:resolve,
   reject:reject
  }}));
 });
 renderPromise.then(function(){
  callbackOutcome='resolved';
  finishCallback();
 },function(){
  callbackOutcome='rejected';
  finishCallback();
 });
 var script=document.createElement('script');
 script.src=RUNNER_URL;
 script.crossOrigin='anonymous';
 script.referrerPolicy='no-referrer';
 script.onload=function(){
  if(terminal)return;
  runnerLoaded=true;
  send({message:'TS APS Runner Loaded',version:1,nonce:EXPECTED_NONCE});
  finishCallback();
 };
 script.onerror=function(){fail('runner_no_load');};
 document.head.appendChild(script);
}
function receiveBind(event){
 if(port||terminal||event.source!==parent||event.origin!=='null'||
    !event.ports||event.ports.length!==1||typeof event.data!=='string'||
    utf8Length(event.data)>MAX_GLOBAL_MESSAGE_BYTES)return;
 var value;
 try{value=JSON.parse(event.data);}catch(_error){return;}
 if(!exactPlain(value,['message','rendererNonce','version'])||
    value.message!=='TS APS Inner Bind'||value.version!==1||
    value.rendererNonce!==EXPECTED_NONCE||
    event.data!==JSON.stringify({message:'TS APS Inner Bind',version:1,
     rendererNonce:EXPECTED_NONCE}))return;
 port=event.ports[0];
 removeEventListener('message',receiveBind);
 port.onmessage=receiveEnvelope;
 port.onmessageerror=function(){fail(accepted?'runner_failed':'descriptor_invalid');};
 try{port.start();}catch(_error){fail('descriptor_invalid');}
}
var match=/^#(n1_[A-Za-z0-9_-]{22})$/.exec(location.hash);
if(!match||match[1]!==EXPECTED_NONCE)return;
try{history.replaceState(null,'',location.pathname+location.search);}catch(_error){
 /* Fragment clearing has no authority over envelope acceptance. */
}
addEventListener('message',receiveBind);
parent.postMessage(JSON.stringify({message:'TS APS Inner Ready',version:1,
 rendererNonce:EXPECTED_NONCE}),'*');
})();
</script>
`;

const OUTER_TEMPLATE = `<!doctype html>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="__TS_OUTER_CSP__">
<style>html,body{border:0;height:100%;margin:0;overflow:hidden;padding:0;width:100%}iframe{border:0;display:block;height:100%;margin:0;overflow:hidden;width:100%}</style>
<body>
<script>
(function(){
'use strict';
var EXPECTED_BOOTSTRAP_NONCE=__TS_BOOTSTRAP_NONCE_JSON__;
var EXPECTED_RENDERER_NONCE=__TS_RENDERER_NONCE_JSON__;
var INNER_URL=__TS_INNER_URL_JSON__;
var PERMANENT_SANDBOX=__TS_PERMANENT_SANDBOX_JSON__;
var MAX_GLOBAL_MESSAGE_BYTES=4096;
var consumed=false;
function utf8Length(value){return(new TextEncoder()).encode(value).length;}
function exact(value,keys){
 if(!value||typeof value!=='object'||Array.isArray(value)||
    Object.getPrototypeOf(value)!==Object.prototype)return false;
 if(typeof Object.getOwnPropertySymbols==='function'&&
    Object.getOwnPropertySymbols(value).length!==0)return false;
 var names=Object.getOwnPropertyNames(value).sort();
 if(names.length!==keys.length)return false;
 for(var index=0;index<names.length;index+=1){
  if(names[index]!==keys[index])return false;
  var descriptor=Object.getOwnPropertyDescriptor(value,names[index]);
  if(!descriptor||!Object.prototype.hasOwnProperty.call(descriptor,'value'))return false;
 }
 return true;
}
var match=/^#(b1_[A-Za-z0-9_-]{22})$/.exec(location.hash);
if(!match||match[1]!==EXPECTED_BOOTSTRAP_NONCE)return;
try{history.replaceState(null,'',location.pathname+location.search);}catch(_error){
 /* Fragment clearing has no authority over channel acceptance. */
}
var frame=document.createElement('iframe');
frame.setAttribute('title','Ad content');
frame.setAttribute('aria-label','Advertisement');
frame.setAttribute('scrolling','no');
frame.setAttribute('frameborder','0');
frame.setAttribute('marginheight','0');
frame.setAttribute('marginwidth','0');
frame.setAttribute('sandbox',PERMANENT_SANDBOX);
frame.src=INNER_URL;
var source=null;
function receive(event){
 if(consumed||!source||event.source!==source||event.origin!=='null'||
    !event.ports||event.ports.length!==0||typeof event.data!=='string'||
    utf8Length(event.data)>MAX_GLOBAL_MESSAGE_BYTES)return;
 var value;
 try{value=JSON.parse(event.data);}catch(_error){return;}
 if(!exact(value,['message','rendererNonce','version'])||
    value.message!=='TS APS Inner Ready'||value.version!==1||
    value.rendererNonce!==EXPECTED_RENDERER_NONCE||
    event.data!==JSON.stringify({message:'TS APS Inner Ready',version:1,
     rendererNonce:EXPECTED_RENDERER_NONCE}))return;
 consumed=true;
 removeEventListener('message',receive);
 var channel=new MessageChannel();
 try{
  source.postMessage(JSON.stringify({message:'TS APS Inner Bind',version:1,
   rendererNonce:EXPECTED_RENDERER_NONCE}),'*',[channel.port1]);
  parent.postMessage(JSON.stringify({message:'TS APS Container Ready',version:1,
   bootstrapNonce:EXPECTED_BOOTSTRAP_NONCE,
   rendererNonce:EXPECTED_RENDERER_NONCE}),'*',[channel.port2]);
 }catch(_error){
  try{channel.port1.close();}catch(_closeError){/* Continue closing the retained endpoint. */}
  try{channel.port2.close();}catch(_closeError){/* Both endpoints are now inert to this owner. */}
 }
}
addEventListener('message',receive);
document.body.appendChild(frame);
source=frame.contentWindow;
})();
</script>
`;

function exactGenerationInput(candidate: unknown): ApsDataDocumentInputV1 | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype
    ) {
      return undefined;
    }
    const expected = ['renderer', 'publisherOrigin', 'bootstrapNonce', 'rendererNonce'];
    const keys = Reflect.ownKeys(candidate);
    if (
      keys.length !== expected.length ||
      keys.some((key) => typeof key !== 'string' || !expected.includes(key))
    ) {
      return undefined;
    }
    const values: Record<string, unknown> = {};
    for (const key of expected) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor?.enumerable || !Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
        return undefined;
      }
      values[key] = descriptor.value;
    }
    if (
      typeof values['publisherOrigin'] !== 'string' ||
      typeof values['bootstrapNonce'] !== 'string' ||
      typeof values['rendererNonce'] !== 'string'
    ) {
      return undefined;
    }
    return {
      renderer: values['renderer'],
      publisherOrigin: values['publisherOrigin'],
      bootstrapNonce: values['bootstrapNonce'],
      rendererNonce: values['rendererNonce'],
    };
  } catch {
    return undefined;
  }
}

function hasForbiddenOriginCharacter(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (
      code <= 0x20 ||
      code === 0x7f ||
      value[index] === "'" ||
      value[index] === '"' ||
      value[index] === ';'
    ) {
      return true;
    }
  }
  return false;
}

function trustedServerOrigin(value: string): string | undefined {
  if (encoder.encode(value).byteLength > 2_048 || hasForbiddenOriginCharacter(value)) {
    return undefined;
  }
  try {
    const origin = new URL(value);
    const loopbackHttp =
      origin.protocol === 'http:' &&
      (origin.hostname === 'localhost' ||
        origin.hostname === '[::1]' ||
        loopbackIpv4Pattern.test(origin.hostname));
    return origin.origin === value &&
      origin.username === '' &&
      origin.password === '' &&
      origin.pathname === '/' &&
      origin.search === '' &&
      origin.hash === '' &&
      (origin.protocol === 'https:' || loopbackHttp)
      ? origin.origin
      : undefined;
  } catch {
    return undefined;
  }
}

function validatedCreativeOrigin(
  renderer: Readonly<ApsRendererV1>,
  publisherOrigin: string
): string | undefined {
  try {
    const creative = new URL(renderer.creativeUrl);
    return creative.protocol === 'https:' &&
      creative.username === '' &&
      creative.password === '' &&
      creative.origin !== publisherOrigin &&
      !hasForbiddenOriginCharacter(creative.origin)
      ? creative.origin
      : undefined;
  } catch {
    return undefined;
  }
}

function replaceSentinel(
  template: string,
  sentinel: string,
  replacement: string
): string | undefined {
  const first = template.indexOf(sentinel);
  if (first < 0 || template.indexOf(sentinel, first + sentinel.length) >= 0) return undefined;
  return `${template.slice(0, first)}${replacement}${template.slice(first + sentinel.length)}`;
}

function substitute(
  template: string,
  replacements: Readonly<Record<string, string>>
): string | undefined {
  let output = template;
  for (const [sentinel, replacement] of Object.entries(replacements)) {
    const next = replaceSentinel(output, sentinel, replacement);
    if (next === undefined) return undefined;
    output = next;
  }
  return sentinelPattern.test(output) ? undefined : output;
}

function htmlAttribute(value: string): string | undefined {
  if (value.includes('&') || value.includes('"') || value.includes('<') || value.includes('>')) {
    return undefined;
  }
  return value;
}

function scriptString(value: string): string | undefined {
  if (!validUnicodeScalars(value)) return undefined;
  return JSON.stringify(value)
    .replace(/</g, '\\u003c')
    .replace(/>/g, '\\u003e')
    .replace(/&/g, '\\u0026')
    .replace(/\u2028/g, '\\u2028')
    .replace(/\u2029/g, '\\u2029');
}

function hasOneClosingScript(documentSource: string): boolean {
  const token = '</script>';
  const first = documentSource.indexOf(token);
  return first >= 0 && first === documentSource.lastIndexOf(token);
}

function validUnicodeScalars(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next < 0xdc00 || next > 0xdfff) return false;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return false;
    }
  }
  return true;
}

function dataUrl(documentSource: string, nonce: string): string {
  return `${APS_DATA_URL_PREFIX}${encodeURIComponent(documentSource)}#${nonce}`;
}

/** Generate one detached outer container and inner renderer before any DOM mutation. */
export function generateApsDataDocumentsV1(
  candidate: unknown
): Readonly<ApsDataDocumentsV1> | undefined {
  try {
    const input = exactGenerationInput(candidate);
    if (
      !input ||
      !bootstrapNoncePattern.test(input.bootstrapNonce) ||
      !rendererNoncePattern.test(input.rendererNonce)
    ) {
      return undefined;
    }
    const trustedOrigin = trustedServerOrigin(input.publisherOrigin);
    if (!trustedOrigin) return undefined;
    const renderer = validateApsRenderer(input.renderer, trustedOrigin);
    if (!renderer) return undefined;
    const creativeOrigin = validatedCreativeOrigin(renderer, trustedOrigin);
    if (!creativeOrigin) return undefined;
    const scriptCreative = renderer.tagType === 'script';
    const selectedOuterCsp = outerCsp(trustedOrigin, creativeOrigin, scriptCreative);
    const selectedInnerCsp = innerCsp(trustedOrigin, creativeOrigin, scriptCreative);
    const runnerUrl = new URL('/integrations/aps/runner.js', trustedOrigin).href;
    const innerCspAttribute = htmlAttribute(selectedInnerCsp);
    const rendererNonceJson = scriptString(input.rendererNonce);
    const publisherOriginJson = scriptString(trustedOrigin);
    const creativeOriginJson = scriptString(creativeOrigin);
    const tagTypeJson = scriptString(renderer.tagType);
    const runnerUrlJson = scriptString(runnerUrl);
    if (
      !innerCspAttribute ||
      !rendererNonceJson ||
      !publisherOriginJson ||
      !creativeOriginJson ||
      !tagTypeJson ||
      !runnerUrlJson
    ) {
      return undefined;
    }
    const innerDocument = substitute(INNER_TEMPLATE, {
      __TS_INNER_CSP__: innerCspAttribute,
      __TS_RENDERER_VALIDATOR__: APS_RENDERER_VALIDATOR_ES5_V1,
      __TS_RENDERER_NONCE_JSON__: rendererNonceJson,
      __TS_PUBLISHER_ORIGIN_JSON__: publisherOriginJson,
      __TS_CREATIVE_ORIGIN_JSON__: creativeOriginJson,
      __TS_TAG_TYPE_JSON__: tagTypeJson,
      __TS_RUNNER_URL_JSON__: runnerUrlJson,
    });
    if (
      !innerDocument ||
      !hasOneClosingScript(innerDocument) ||
      encoder.encode(innerDocument).byteLength > MAX_APS_INNER_DOCUMENT_BYTES
    ) {
      return undefined;
    }
    const innerUrl = dataUrl(innerDocument, input.rendererNonce);
    const outerCspAttribute = htmlAttribute(selectedOuterCsp);
    const bootstrapNonceJson = scriptString(input.bootstrapNonce);
    const innerUrlJson = scriptString(innerUrl);
    const permanentSandboxJson = scriptString(APS_PERMANENT_SANDBOX);
    if (!outerCspAttribute || !bootstrapNonceJson || !innerUrlJson || !permanentSandboxJson) {
      return undefined;
    }
    const outerDocument = substitute(OUTER_TEMPLATE, {
      __TS_OUTER_CSP__: outerCspAttribute,
      __TS_BOOTSTRAP_NONCE_JSON__: bootstrapNonceJson,
      __TS_RENDERER_NONCE_JSON__: rendererNonceJson,
      __TS_INNER_URL_JSON__: innerUrlJson,
      __TS_PERMANENT_SANDBOX_JSON__: permanentSandboxJson,
    });
    if (
      !outerDocument ||
      !hasOneClosingScript(outerDocument) ||
      encoder.encode(outerDocument).byteLength > MAX_APS_CONTAINER_DOCUMENT_BYTES
    ) {
      return undefined;
    }
    const outerUrl = dataUrl(outerDocument, input.bootstrapNonce);
    if (encoder.encode(outerUrl).byteLength > MAX_APS_CONTAINER_URL_BYTES) return undefined;
    return Object.freeze({
      version: 1,
      bootstrapNonce: input.bootstrapNonce,
      rendererNonce: input.rendererNonce,
      trustedServerOrigin: trustedOrigin,
      creativeOrigin,
      sandbox: APS_PERMANENT_SANDBOX,
      outerCsp: selectedOuterCsp,
      innerCsp: selectedInnerCsp,
      outerDocument,
      innerDocument,
      outerUrl,
      innerUrl,
    });
  } catch {
    return undefined;
  }
}
