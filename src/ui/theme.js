// Runs before the stylesheet: the stored theme choice (or the OS setting) is applied before the first paint,
// so a dark page never flashes white. Keep the two paper colours in step with app.css.
(function () {
  var stored = null;
  try { stored = localStorage.getItem("bq-theme"); } catch (e) {}
  var dark = stored === "dark"; // light is the default; dark only when chosen with the toggle
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  var tc = document.querySelector('meta[name="theme-color"]');
  if (tc) tc.setAttribute("content", dark ? "#0e0e0e" : "#ffffff");
})();
