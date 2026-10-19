// Minimal browser UI for manual testing (login, list buckets/objects, download).
// All URLs are relative to <base href> so the page also works under a path prefix.
// Bucket registration and ACL management are done through the API / CLI.
/** `basePath` is the path prefix of PUBLIC_BASE_URL (e.g. an API Gateway stage), or ''. */
export const indexHtml = (basePath: string) => `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<base href="${basePath.replace(/[^A-Za-z0-9/_-]/g, '')}/">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>CARLA Content Pack Registry</title>
<style>
  body { font: 15px/1.5 system-ui, sans-serif; max-width: 960px; margin: 2rem auto; padding: 0 1rem; color: #1b1f24; }
  header { display: flex; justify-content: space-between; align-items: center; gap: 1rem; }
  table { border-collapse: collapse; width: 100%; margin: 1rem 0; }
  th, td { text-align: left; padding: .35rem .5rem; border-bottom: 1px solid #d0d7de; font-size: 14px; }
  code { background: #f6f8fa; padding: 0 .25rem; border-radius: 4px; }
  .tag { font-size: 12px; padding: 0 .4rem; border-radius: 8px; background: #eaeef2; }
  button, a.btn { font: inherit; padding: .3rem .8rem; border: 1px solid #d0d7de; border-radius: 6px; background: #f6f8fa; cursor: pointer; text-decoration: none; color: inherit; }
  #err { color: #cf222e; }
</style>
</head>
<body>
<header><h1>CARLA Content Pack Registry</h1><div id="who"></div></header>
<p id="err"></p>
<section id="buckets"></section>
<section id="objects"></section>
<script src="app.js"></script>
</body>
</html>`;

export const APP_JS = `'use strict';
let csrf = null;
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
async function api(path, opts = {}) {
  const headers = Object.assign({ accept: 'application/json' }, opts.headers || {});
  if (csrf && opts.method && opts.method !== 'GET') headers['x-csrf-token'] = csrf;
  const r = await fetch(path, Object.assign({}, opts, { headers, credentials: 'same-origin' }));
  if (r.status === 204) return null;
  const body = await r.json();
  if (!r.ok) throw new Error(body.error ? body.error.code + ': ' + body.error.message : 'HTTP ' + r.status);
  return body;
}
async function loadMe() {
  try {
    const me = await api('v1/me');
    csrf = me.csrfToken;
    $('who').innerHTML = 'Signed in as <b>' + esc(me.user.login) + '</b> <button id="logout">Sign out</button>';
    $('logout').onclick = async () => { await api('logout', { method: 'POST' }); location.reload(); };
    if (me.membershipError) $('err').textContent = 'GitHub membership check failed: ' + me.membershipError;
    return true;
  } catch (e) {
    $('who').innerHTML = '<a class="btn" href="login/github">Sign in with GitHub</a>';
    return false;
  }
}
async function loadBuckets() {
  const { buckets } = await api('v1/buckets');
  const rows = buckets.map((b) =>
    '<tr><td><a href="#" data-id="' + esc(b.id) + '">' + esc(b.name) + '</a></td><td>' + esc(b.owner.login) +
    '</td><td><span class="tag">' + esc(b.visibility) + '</span></td><td>' + esc(b.provider) + '</td><td>' + esc(b.verifyStatus) + '</td></tr>').join('');
  $('buckets').innerHTML = '<h2>Buckets</h2><table><tr><th>Name</th><th>Owner</th><th>Visibility</th><th>Provider</th><th>Status</th></tr>' + rows + '</table>';
  for (const a of $('buckets').querySelectorAll('a[data-id]')) a.onclick = (ev) => { ev.preventDefault(); loadObjects(a.dataset.id, a.textContent); };
}
async function loadObjects(id, name) {
  $('err').textContent = '';
  try {
    const page = await api('v1/buckets/' + id + '/objects?limit=200');
    const rows = page.objects.map((o) =>
      '<tr><td><code>' + esc(o.key) + '</code></td><td>' + o.size + '</td><td><button data-key="' + esc(o.key) + '">Download</button></td></tr>').join('');
    $('objects').innerHTML = '<h2>' + esc(name) + '</h2><table><tr><th>Key</th><th>Size</th><th></th></tr>' + rows + '</table>';
    for (const b of $('objects').querySelectorAll('button[data-key]')) b.onclick = async () => {
      try {
        const u = await api('v1/buckets/' + id + '/objects/' + b.dataset.key.split('/').map(encodeURIComponent).join('/') + '/download');
        location.href = u.url;
      } catch (e) { $('err').textContent = e.message; }
    };
  } catch (e) { $('err').textContent = e.message; }
}
(async () => { if (await loadMe()) { try { await loadBuckets(); } catch (e) { $('err').textContent = e.message; } } })();
`;
