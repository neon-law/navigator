(function () {
  "use strict";
  function init() {
    var dialog = document.getElementById("signup-prompt");
    if (!dialog || location.pathname !== "/" || typeof dialog.showModal !== "function") return;
    var key = "navigator.signup.offered";
    var offered = false;
    try { offered = sessionStorage.getItem(key) === "true"; } catch (_) { /* Storage is optional. */ }
    if (offered) return;
    var previousFocus;
    function close() { dialog.close(); }
    dialog.querySelector("[data-signup-dismiss]").addEventListener("click", close);
    dialog.addEventListener("click", function (event) { if (event.target === dialog) close(); });
    dialog.addEventListener("close", function () {
      if (previousFocus && previousFocus.isConnected) previousFocus.focus();
    });
    var timer;
    function offer() {
      if (document.hidden || document.querySelector("dialog[open]")) {
        timer = setTimeout(offer, 1000);
        return;
      }
      if (location.pathname !== "/") return;
      previousFocus = document.activeElement;
      try { sessionStorage.setItem(key, "true"); } catch (_) { /* Storage is optional. */ }
      dialog.showModal();
      dialog.querySelector("[data-signup-dismiss]").focus();
    }
    timer = setTimeout(offer, 10000);
    window.addEventListener("pagehide", function () { clearTimeout(timer); }, { once: true });
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
