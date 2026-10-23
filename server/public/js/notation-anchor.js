// notation-anchor.js — a deep link into a closed disclosure opens it.
//
// A Notation renders as a native <details>, and every unit and paragraph in
// it carries a path-derived id. Chromium opens a closed <details> when the
// fragment lands inside it; other browsers do not. This file opens every
// ancestor <details> of the element `location.hash` names, on load and on
// `hashchange`, then scrolls to it. The content needs no script: without
// this file every paragraph is still there and every link still resolves.

(function () {
  "use strict";

  if (window.__navNotationAnchor) {
    return;
  }
  window.__navNotationAnchor = true;

  function target() {
    var raw = location.hash.slice(1);
    if (!raw) {
      return null;
    }
    try {
      return document.getElementById(decodeURIComponent(raw));
    } catch (e) {
      return document.getElementById(raw);
    }
  }

  function openAncestors() {
    var element = target();
    if (!element) {
      return;
    }
    var opened = false;
    for (var node = element; node; node = node.parentElement) {
      if (node.tagName === "DETAILS" && !node.open) {
        node.open = true;
        opened = true;
      }
    }
    if (opened) {
      element.scrollIntoView();
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", openAncestors);
  } else {
    openAncestors();
  }
  window.addEventListener("hashchange", openAncestors);
})();
