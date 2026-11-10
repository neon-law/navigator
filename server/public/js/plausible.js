// Queue Plausible analytics calls until the vendor script arrives, and report
// the clicks that turn a visit into a call or a booking.
//
// This is the inline stub from Plausible's published snippet, served as a
// first-party file so it needs no per-response nonce. It runs as the first of
// two deferred scripts the render middleware emits (see
// `portal::plausible::PlausibleSite::script_tags`); deferred classic scripts
// execute in document order, so `init()` is queued before the vendor script
// runs and drains it.
//
// `init()` turns on Plausible's own outbound-link ("Outbound Link: Click")
// and form-submission ("Form: Submission") events. The click listener adds
// the two goals Plausible cannot infer: a `tel:` link ("Phone: Clicked") and
// the brand's booking link ("Booking: Clicked"), whose URL the loader tag
// carries as `data-booking`. Plausible records each event's page, and the
// visit's source and `utm_*` campaign, itself; the props name only the link
// followed — the firm's own number or URL — never anything the visitor typed.
//
// Any element marked `data-funnel-event="<Goal>"` reports that goal when
// clicked, so a funnel step is declared in the markup that renders it. The
// event carries no props: Plausible's own page and source dimensions are the
// segments.
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

(function () {
  var script = document.currentScript;
  var booking = script && script.dataset.booking;
  document.addEventListener(
    "click",
    function (event) {
      var named = event.target.closest && event.target.closest("[data-funnel-event]");
      if (named) plausible(named.dataset.funnelEvent);
      var link = event.target.closest && event.target.closest("a[href]");
      if (!link) return;
      if (link.protocol === "tel:") {
        plausible("Phone: Clicked", {
          props: { number: link.href.slice(4) },
        });
      } else if (
        (booking && link.href === new URL(booking, location.href).href) ||
        (link.host === location.host && link.pathname === "/consultation")
      ) {
        plausible("Booking: Clicked", {
          props: { url: link.href },
        });
      }
    },
    true
  );
})();
