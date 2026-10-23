//! Translating the body (#327): the scripts that take the text out of the
//! editor and put the translation back.

/// Which editor a translation (#327) was taken from, and so how it goes
/// back: as HTML into the rich document, or as text into the source field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateHow {
    Rich,
    Markdown,
    Html,
}

/// The quoted message's text, for telling which language to offer first:
/// the reply's quote in the rich document, or the quoted lines of a
/// Markdown or HTML source.
pub(super) const TR_QUOTE_TEXT: &str = r#"(function(){var t=document.getElementById('src');
if(t){var v=t.value;var i=v.search(/<blockquote/i);
if(i>=0)return v.slice(i).replace(/<[^>]*>/g,' ').slice(0,4000);
return v.split('\n').filter(function(l){return /^\s*>/.test(l);}).map(function(l){return l.replace(/^\s*>+\s?/,'');}).join('\n').slice(0,4000);}
var q=document.querySelector('blockquote.hylki-quote-mail')||document.querySelector('blockquote');
return q?q.innerText.slice(0,4000):'';})()"#;

/// What the rich document asks to have translated: the selection, else
/// what was written above the quote and signature. The range is kept for
/// the answer, and the body is locked until it comes. Block elements go as
/// pieces of their own; runs of inline content go together. The range
/// starts and ends inside the first and last blocks, not around them: an
/// insert over whole blocks runs its last paragraph into the quote's
/// "wrote:" line.
pub(super) const TR_RICH_BEGIN: &str = r#"(function(){var b=document.body;if(!b)return '';
var s=getSelection(),r=null,sel=false;
if(s.rangeCount&&!s.isCollapsed&&b.contains(s.anchorNode)){r=s.getRangeAt(0).cloneRange();sel=true;}
else{var first=null,c=b.querySelectorAll('.hylki-sig,.hylki-quote-attr,blockquote');
for(var i=0;i<c.length;i++){var t=c[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;
if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}
var kids=[];for(var n=b.firstChild;n&&n!==first;n=n.nextSibling)kids.push(n);
var full=function(n){return !!(n.textContent.trim()||(n.nodeType===1&&(n.tagName==='IMG'||n.querySelector('img'))));};
var a=0,e=kids.length;while(a<e&&!full(kids[a]))a++;while(e>a&&!full(kids[e-1]))e--;
if(a>=e)return JSON.stringify({empty:true,sel:false});
r=document.createRange();
var A=kids[a],E=kids[e-1];
if(A.nodeType===1)r.setStart(A,0);else r.setStartBefore(A);
if(E.nodeType===1)r.setEnd(E,E.childNodes.length);else r.setEndAfter(E);}
var f=r.cloneContents();if(!f.textContent.trim())return JSON.stringify({empty:true,sel:sel});
var p=[];
if(sel){var w=document.createElement('div');w.appendChild(f);p.push(w.innerHTML);}
else{var BLOCK=/^(P|DIV|UL|OL|LI|H[1-6]|PRE|TABLE|BLOCKQUOTE|HR)$/,run='';
var flush=function(){if(run.trim())p.push(run);run='';};
for(var n=f.firstChild;n;n=n.nextSibling){
if(n.nodeType===1&&BLOCK.test(n.tagName)){flush();p.push(n.outerHTML);}
else if(n.nodeType===1){run+=n.outerHTML;}
else if(n.nodeType===3){var d=document.createElement('div');d.textContent=n.textContent;run+=d.innerHTML;}}
flush();}
window.__hylkiTr=r;b.setAttribute('contenteditable','false');
return JSON.stringify({parts:p,sel:sel});})()"#;

/// The translation in place of what was taken, as one edit the editor's
/// undo can take back. `__hylkiTrState` then follows it for the Translate
/// menu (#327): "translated", "original" once undone, and nothing once
/// anything else is done to the text.
pub(super) const TR_RICH_END: &str = r#"(function(h){var b=document.body;if(!b)return;b.setAttribute('contenteditable','true');
var r=window.__hylkiTr;window.__hylkiTr=null;if(!r)return;b.focus();
var s=getSelection();s.removeAllRanges();s.addRange(r);
document.execCommand('insertHTML',false,h);window.__hylkiPlainFix&&window.__hylkiPlainFix();window.__hylkiDirty=true;window.__hylkiTrState='translated';if(!window.__hylkiTrWatch){window.__hylkiTrWatch=true;document.addEventListener('input',function(e){var st=window.__hylkiTrState;if(!st)return;var k=e.inputType||'';if(k==='historyUndo')window.__hylkiTrState=st==='translated'?'original':'';else if(k==='historyRedo')window.__hylkiTrState=st==='original'?'translated':'';else window.__hylkiTrState='';},true);}})(ARG)"#;

/// The source field's version of [`TR_RICH_BEGIN`]: the selection, else
/// the text above the quote (and, in Markdown, its "wrote:" line) and the
/// signature. HTMLFLAG says whether the source is HTML.
pub(super) const TR_SOURCE_BEGIN: &str = r#"(function(H){var t=document.getElementById('src');if(!t)return '';
var v=t.value,a=t.selectionStart,e=t.selectionEnd,sel=e>a;
if(!sel){a=0;e=v.length;
var sig=H?v.search(/<div class="hylki-sig"/):v.indexOf('\n-- \n');if(sig>=0)e=Math.min(e,sig);
var q=H?v.search(/<p class="hylki-quote-attr"|<blockquote/i):v.search(/(^|\n)[ \t]*>/);
if(q>=0){if(!H){var pre=v.slice(0,q).replace(/\s+$/,'');var ls=pre.lastIndexOf('\n');
if(/(wrote|schrieb|a écrit|escribió|Forwarded message)/i.test(pre.slice(ls+1)))q=ls+1;}
e=Math.min(e,q);}}
while(e>a&&/\s/.test(v.charAt(e-1)))e--;while(a<e&&/\s/.test(v.charAt(a)))a++;
var x=v.slice(a,e);if(!x.trim())return JSON.stringify({empty:true,sel:sel});
window.__hylkiTr={a:a,e:e};t.readOnly=true;return JSON.stringify({text:x,sel:sel});})(HTMLFLAG)"#;

/// [`TR_RICH_END`] for the source field.
pub(super) const TR_SOURCE_END: &str = r#"(function(x){var t=document.getElementById('src');if(!t)return;t.readOnly=false;
var r=window.__hylkiTr;window.__hylkiTr=null;if(!r)return;t.focus();t.setSelectionRange(r.a,r.e);
if(!document.execCommand('insertText',false,x))t.setRangeText(x,r.a,r.e,'end');
window.__hylkiDirty=true;t.dispatchEvent(new Event('input'));window.__hylkiTrState='translated';if(!window.__hylkiTrWatch){window.__hylkiTrWatch=true;document.addEventListener('input',function(e){var st=window.__hylkiTrState;if(!st)return;var k=e.inputType||'';if(k==='historyUndo')window.__hylkiTrState=st==='translated'?'original':'';else if(k==='historyRedo')window.__hylkiTrState=st==='original'?'translated':'';else window.__hylkiTrState='';},true);}})(ARG)"#;

/// A translation that will not come: unlock the body as it was.
pub(super) const TR_CANCEL: &str = r#"(function(){var b=document.body;if(b&&!document.getElementById('src'))b.setAttribute('contenteditable','true');
var t=document.getElementById('src');if(t)t.readOnly=false;window.__hylkiTr=null;})()"#;
