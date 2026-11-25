// Starts the vendored CLax overlay for a lawyer-tier session. The portal
// injects this tag with the queue, instance, person and CSRF token as data
// attributes; the overlay's reporter token comes from our own token route.
(function () {
  "use strict";
  var tag = document.currentScript;
  if (!tag) return;
  var data = tag.dataset;
  var overlay = document.createElement("script");
  overlay.src = "/public/vendor/clax/overlay.js";
  overlay.onload = function () {
    if (!window.Clax) return;
    window.Clax.overlay({
      instance: data.instance,
      queue: data.queue,
      user: data.user,
      auth: function (publicKey) {
        return fetch("/app/api/clax/token", {
          method: "POST",
          headers: { "content-type": "application/json", "x-csrf-token": data.csrf },
          body: JSON.stringify({ public_key: publicKey }),
        }).then(function (r) {
          if (r.ok) return r.json();
          return r.json().catch(function () { return null; }).then(function (body) {
            var code = body && body.error && body.error.code;
            throw Object.assign(new Error("no reporter token: " + (code || r.status)), { code: code });
          });
        });
      },
      context: function () {
        return { app: { path: location.pathname } };
      },
      nonce: "strict",
    });
  };
  document.head.appendChild(overlay);
})();
