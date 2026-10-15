// Scripts injected by /__shot?demo=<name> (same-origin module, so the strict CSP still holds). They drive the UI so
// screenshots can capture interactive states (diff, confirm dialogs, conflicts, streaming...). Dev only.
const W = `const until=async(f,n=100)=>{for(let i=0;i<n;i++){const v=f();if(v)return v;await new Promise(r=>setTimeout(r,100));}return null;};
const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const $=(s)=>document.querySelector(s); const $$=(s)=>[...document.querySelectorAll(s)];
const btn=(txt,root=document)=>[...root.querySelectorAll('button,a.btn')].find(b=>b.textContent.trim()===txt);
const typeInto=(el,v)=>{el.value=v;el.dispatchEvent(new Event('input',{bubbles:true}));el.dispatchEvent(new Event('change',{bubbles:true}));};
const editorReady=async()=>{const ta=await until(()=>$('textarea.code-edit'));await sleep(300);return ta;};
`;
export const DEMOS = {
  'config-diff': W + `const ta=await editorReady(); typeInto(ta, ta.value.replace('confidence_threshold = 0.5\\nrecent_turn_window = 3','confidence_threshold = 0.65\\nrecent_turn_window = 5').replace('# PRIMARY. Fully local.','# PRIMARY. Fully local.\\n# raised the threshold after the 2026-09-23 A/B')); await sleep(300); ta.setSelectionRange(0,0);`,
  'config-diff-unified': W + `const ta=await editorReady(); typeInto(ta, ta.value.replace('picker = "efficient_first"','picker = "capable_first"')); const s=$('select[aria-label]'); s.value='unified'; s.dispatchEvent(new Event('change'));`,
  'config-invalid': W + `const ta=await editorReady(); typeInto(ta, ta.value.replace('capable_target = "flash-next-auto"','capable_target = "nope"')); btn('Validate').click(); await until(()=>$('.validation'));`,
  'config-confirm': W + `const ta=await editorReady(); typeInto(ta, ta.value.replace('confidence_threshold = 0.5','confidence_threshold = 0.7')); btn('Apply').click(); await until(()=>$('dialog[open]'));`,
  'config-rollback': W + `const ta=await editorReady(); typeInto(ta, ta.value+'\\n# unhealthy: routes to a target that never answers\\n'); btn('Apply').click(); await until(()=>$('dialog[open]')); btn('Apply', $('dialog')).click(); await until(()=>$('.outcome'));`,
  'config-conflict': W + `const ta=await editorReady(); await fetch('/__dev/touch',{headers:{'X-Console':'1'}}); typeInto(ta, ta.value.replace('confidence_threshold = 0.5','confidence_threshold = 0.7')); btn('Apply').click(); await until(()=>$('dialog[open]')); btn('Apply', $('dialog')).click(); await until(()=>$('.conflict'));`,
  'config-success-history': W + `const ta=await editorReady(); typeInto(ta, ta.value.replace('confidence_threshold = 0.5','confidence_threshold = 0.55')); btn('Apply').click(); await until(()=>$('dialog[open]')); btn('Apply', $('dialog')).click(); await until(()=>$('.outcome')); await sleep(400); btn('History').click(); await until(()=>$('table .btn'));`,
  'relay-routes': W + `const ta=await editorReady(); typeInto(ta, ta.value.replace('confidence_threshold = 0.5','confidence_threshold = 0.6')); btn('Validate').click(); await until(()=>$('.validation'));`,
  'relay-plugins': W + `const b=await until(()=>$('.seg-control button[data-file=plugins]')); b.click(); await until(()=>$('.readonly')); await sleep(200);`,
  'playground': W + `const ta=await until(()=>$('#pg-prompt')); await sleep(300); ta.value='Explain in two sentences how tier routing works.'; btn('Send').click(); await until(()=>$('.pg-log .msg.assistant')); await sleep(900);`,
  'playground-done': W + `const ta=await until(()=>$('#pg-prompt')); await sleep(300); ta.value='Explain in two sentences how tier routing works.'; btn('Send').click(); await sleep(2600); await until(()=>$('.pg-log .msg.assistant') && !$('.streaming'));`,
  'sy-action-confirm': W + `const b=await until(()=>btn('Reset stats')); b.click(); await until(()=>$('dialog[open]'));`,
  'relay-doctor': W + `const b=await until(()=>btn('Doctor (offline)')); b.click(); await until(()=>$('.action-result'));`,
  'dyn-params': W + `const g=await until(()=>$('#p-gpu_memory_utilization')); await sleep(400); typeInto(g,'0.97'); typeInto($('#p-max_num_seqs'),'32'); const k=$('#p-kv_cache_dtype'); k.value='fp8'; k.dispatchEvent(new Event('change')); await sleep(900);`,
  'dyn-params-invalid': W + `const g=await until(()=>$('#p-gpu_memory_utilization')); await sleep(400); typeInto(g,'1.5'); await sleep(300);`,
  'dyn-params-confirm': W + `const g=await until(()=>$('#p-max_num_seqs')); await sleep(500); typeInto(g,'32'); await sleep(600); btn('Apply parameters').click(); await until(()=>$('dialog[open]'));`,
  'dyn-lifecycle-confirm': W + `await until(()=>$('.preflight-hero')); const up=btn('Up'); up.click(); await until(()=>$('dialog[open]')); await sleep(300);`,
  'dyn-lifecycle-down-confirm': W + `await until(()=>$('.preflight-hero')); btn('Down').click(); await until(()=>$('dialog[open]')); await sleep(300);`,
  'audit-fill': W + `for (const p of [['/api/v1/systems/switchyard/actions/dry_run_config',{params:{},confirm:false}],['/api/v1/systems/switchyard/actions/reset_stats',{params:{},confirm:true}],['/api/v1/systems/relay/actions/doctor_offline',{params:{},confirm:false}]]) await fetch(p[0],{method:'POST',headers:{'X-Console':'1','Content-Type':'application/json'},body:JSON.stringify(p[1])}); location.hash='#/audit?x=1'; await sleep(300); location.hash='#/audit';`,
};
