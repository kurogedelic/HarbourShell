// Harbour Shell web demo: picture size from the page, pretend hardware, and a home folder kept in
// the browser (IndexedDB), so notes, drawings and settings survive a reload.
var Module = typeof Module !== 'undefined' ? Module : {};
Module.preRun = Module.preRun || [];
Module.preRun.push(function () {
  var q = new URLSearchParams(location.search);
  var vw = window.innerWidth, vh = window.innerHeight;
  // ?size=320x170 shows the Cardputer screen as it is; otherwise the page decides.
  var size = (q.get('size') || '').match(/^(\d+)x(\d+)$/);
  var scale = parseInt(q.get('scale') || '0', 10);
  var w, h;
  if (size) {
    w = +size[1]; h = +size[2];
    if (!scale) scale = Math.max(1, Math.min(6, Math.floor(Math.min(vw / w, vh / h))));
  } else {
    if (!scale) scale = vw >= 1400 && vh >= 800 ? 2 : 1;
    if (vw < 700) scale = Math.max(1, Math.floor(Math.min(vw / 320, vh / 170)));
    w = Math.max(320, Math.floor(vw / scale)); h = Math.max(170, Math.floor(vh / scale));
  }
  ENV.HARBOUR_SCREEN = w + 'x' + h;
  ENV.HARBOUR_SCALE = String(scale);
  ENV.HARBOUR_MOCK = '1';
  if (q.get('mode')) ENV.HARBOUR_MODE = q.get('mode');
  if (q.get('lang')) ENV.LANG = q.get('lang');
  // ?script=goto:Paint,Return,... drives the shell like the scripted runs (see HARBOUR_SCRIPT).
  if (q.get('script')) ENV.HARBOUR_SCRIPT = q.get('script');
  ENV.HOME = '/home/web_user';
  try { FS.mkdirTree('/home/web_user'); } catch (e) {}
  FS.mount(IDBFS, {}, '/home/web_user');
  addRunDependency('idbfs');
  FS.syncfs(true, function (err) {
    if (err) console.warn('home folder: ' + err);
    removeRunDependency('idbfs');
  });
  var save = function () { FS.syncfs(false, function () {}); };
  setInterval(save, 4000);
  window.addEventListener('pagehide', save);
  document.addEventListener('visibilitychange', function () { if (document.hidden) save(); });
});
