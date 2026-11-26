// Plausible analytics loader (added 2026-11-26, Waleed: analytics on, as on the live Navigator site).
// Copied from Navigator's /public/js/plausible.js, minus its booking-link goal (this site has no booking link).
// Queues Plausible calls until the vendor script (plausible.io, same site ID as the live site) arrives, turns on
// Plausible's outbound-link and form-submission events, and reports clicks on a tel: link as "Phone: Clicked".
// Plausible sets no cookies and records nothing the visitor types. The export adds this file and the vendor script
// to the live pages only; the local preview never loads them.
window.plausible =
  window.plausible ||
  function () {
    (plausible.q = plausible.q || []).push(arguments);
  };
plausible.init =
  plausible.init ||
  function (options) {
    plausible.o = options || {};
  };
plausible.init({ outboundLinks: true, formSubmissions: true });

document.addEventListener(
  "click",
  function (event) {
    var link = event.target.closest && event.target.closest("a[href]");
    if (link && link.protocol === "tel:") {
      plausible("Phone: Clicked", { props: { number: link.href.slice(4) } });
    }
  },
  true
);
