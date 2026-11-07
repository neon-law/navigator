// keyboard-shortcuts.js — the browser half of `webapp::keyboard`.
//
// The Rust registry declares every shortcut as JSON on a `[data-nav-shortcuts]`
// element: a key, a description, a scope and an action. This file collects those
// declarations, listens for keys, runs the action a matching declaration names,
// and fills the `?` overlay from the same list — so a key that works is a key
// that is listed, and there is no second table here. `script-src` forbids an
// inline handler, so the listener is delegated from the document.
//
// Key notation (the same as `webapp::keyboard`): `?`, `1`, `Mod+Enter`; a chord is
// keys separated by spaces (`g m`); `1-9` is one declaration for the digit row.
// A bare key never fires while focus is in a text field; a declaration opts in
// with `in_fields`. A half-typed chord lapses after `data-chord-timeout-ms`.
// The `?` overlay and the `/` palette are both native dialogs; exactly one of
// them owns the keyboard at a time, and closing either restores prior focus.
//
// With this script unavailable every page still works with the pointer.

(function () {
  "use strict";

  if (window.__navKeyboardShortcuts) {
    return;
  }
  window.__navKeyboardShortcuts = true;

  var DECLARATIONS = "[data-nav-shortcuts]";
  var DIALOG_ID = "nav-shortcuts-dialog";
  var PALETTE_ID = "nav-palette-dialog";
  var PALETTE_FILTER = "data-nav-palette-filter";
  var PALETTE_RESULTS = "data-nav-palette-results";
  var PALETTE_CLOSE = "data-nav-palette-close";
  var NON_TEXT_INPUTS = {
    button: true,
    checkbox: true,
    color: true,
    file: true,
    image: true,
    radio: true,
    range: true,
    reset: true,
    submit: true,
  };
  var MODIFIER_KEYS = {
    Shift: true,
    Control: true,
    Alt: true,
    Meta: true,
    AltGraph: true,
    CapsLock: true,
  };
  var SYMBOLS = {
    ArrowLeft: "←",
    ArrowRight: "→",
    ArrowUp: "↑",
    ArrowDown: "↓",
    Backspace: "⌫",
    Escape: "Esc",
  };
  var SCOPES = [
    { scope: "global", heading: "Everywhere" },
    { scope: "page", heading: "On this page" },
  ];

  var pending = null;
  var timer = null;
  var returnFocus = null;
  var paletteReturnFocus = null;
  var selectedRow = null;

  function isApple() {
    return /Mac|iPhone|iPad|iPod/.test(navigator.platform || "");
  }

  function declarations() {
    var all = [];
    var hosts = document.querySelectorAll(DECLARATIONS);
    for (var i = 0; i < hosts.length; i += 1) {
      try {
        var parsed = JSON.parse(hosts[i].getAttribute("data-nav-shortcuts"));
        for (var j = 0; j < parsed.length; j += 1) {
          all.push(parsed[j]);
        }
      } catch (err) {
        // A malformed block is skipped; the others still work.
      }
    }
    return all;
  }

  function chordTimeout() {
    var host = document.querySelector("[data-chord-timeout-ms]");
    var ms = host ? parseInt(host.getAttribute("data-chord-timeout-ms"), 10) : NaN;
    return ms > 0 ? ms : 1000;
  }

  function steps(spec) {
    return spec.split(" ").filter(function (step) {
      return step !== "";
    });
  }

  function parseStep(spec) {
    var plus = spec === "+" || spec.slice(-2) === "++";
    var key = plus ? "+" : spec.slice(spec.lastIndexOf("+") + 1);
    var head = plus ? spec.slice(0, -1) : spec.slice(0, Math.max(spec.lastIndexOf("+"), 0));
    var names = head === "" ? [] : head.toLowerCase().split("+");
    return {
      mod: names.indexOf("mod") !== -1,
      alt: names.indexOf("alt") !== -1,
      shift: names.indexOf("shift") !== -1,
      key: key,
    };
  }

  function matchesStep(spec, event) {
    var parsed = parseStep(spec);
    var mod = event.metaKey || event.ctrlKey;
    if (parsed.mod !== mod || parsed.alt !== event.altKey) {
      return false;
    }
    var range = /^(\d)-(\d)$/.exec(parsed.key);
    if (parsed.key.length > 1 && !range && parsed.shift !== event.shiftKey) {
      return false;
    }
    if (range) {
      return event.key.length === 1 && event.key >= range[1] && event.key <= range[2];
    }
    if (parsed.key.length === 1) {
      // Shift+G is not `g`: only a letter written in capitals asks for Shift.
      var lower = parsed.key.toLowerCase();
      var letter = lower !== parsed.key.toUpperCase();
      if (letter && parsed.key === lower && event.shiftKey) {
        return false;
      }
      return event.key.toLowerCase() === lower;
    }
    return event.key === parsed.key;
  }

  function isEditable(target) {
    if (!target || target.nodeType !== 1) {
      return false;
    }
    if (target.isContentEditable) {
      return true;
    }
    var tag = target.tagName;
    if (tag === "TEXTAREA" || tag === "SELECT") {
      return true;
    }
    if (tag === "INPUT") {
      return !NON_TEXT_INPUTS[(target.getAttribute("type") || "text").toLowerCase()];
    }
    return false;
  }

  function reset() {
    pending = null;
    if (timer !== null) {
      window.clearTimeout(timer);
      timer = null;
    }
  }

  function advance(pool, matched, event, editable) {
    return pool.filter(function (shortcut) {
      if (editable && !shortcut.in_fields) {
        return false;
      }
      var step = steps(shortcut.keys)[matched];
      return step !== undefined && matchesStep(step, event);
    });
  }

  /* ----------------------------------------------------------- actions -- */

  function questionForm() {
    return document.querySelector("form[data-nav-questionnaire]");
  }

  function choices(form) {
    var inputs = form ? form.querySelectorAll("input[type=radio], input[type=checkbox]") : [];
    var usable = [];
    for (var i = 0; i < inputs.length; i += 1) {
      if (!inputs[i].disabled) {
        usable.push(inputs[i]);
      }
    }
    return usable;
  }

  function firstInput(form) {
    return form
      ? form.querySelector(
          "input:not([type=hidden]):not(:disabled), select:not(:disabled), textarea:not(:disabled)"
        )
      : null;
  }

  function run(shortcut, event) {
    switch (shortcut.action) {
      case "help":
        openHelp();
        break;
      case "open_palette":
        openPalette();
        break;
      case "navigate":
        if (shortcut.href) {
          window.location.assign(shortcut.href);
        }
        break;
      case "next": {
        var form = questionForm();
        if (form) {
          if (typeof form.requestSubmit === "function") {
            form.requestSubmit();
          } else {
            form.submit();
          }
        }
        break;
      }
      case "back": {
        var backForm = questionForm();
        if (backForm) {
          var backHref = backForm.getAttribute("data-nav-back-href");
          var backUrl = backHref ? new URL(backHref, window.location.href) : null;
          if (backUrl && backUrl.origin === window.location.origin) {
            window.location.assign(backUrl.href);
          } else {
            var action = backForm.querySelector("[data-nav-action]");
            if (action) action.value = "back";
            // Back must work even when the current required answer is blank.
            backForm.noValidate = true;
            if (typeof backForm.requestSubmit === "function") backForm.requestSubmit();
            else backForm.submit();
          }
        }
        break;
      }
      case "choose": {
        var option = choices(questionForm())[Number(event.key) - 1];
        if (option) {
          option.click();
        }
        break;
      }
      default:
        break;
    }
  }

  /* ----------------------------------------------------------- overlay -- */

  function formatStep(spec) {
    var parsed = parseStep(spec);
    var apple = isApple();
    var caps = [];
    if (parsed.mod) {
      caps.push(apple ? "⌘" : "Ctrl");
    }
    if (parsed.alt) {
      caps.push(apple ? "⌥" : "Alt");
    }
    if (parsed.shift) {
      caps.push(apple ? "⇧" : "Shift");
    }
    caps.push(SYMBOLS[parsed.key] || parsed.key.replace("-", "–"));
    return caps;
  }

  function renderList(list, shortcuts) {
    list.textContent = "";
    SCOPES.forEach(function (group) {
      var rows = shortcuts.filter(function (shortcut) {
        return shortcut.scope === group.scope;
      });
      if (rows.length === 0) {
        return;
      }
      var section = document.createElement("section");
      section.className = "nav-shortcuts";
      var headingId = "nav-shortcuts-" + group.scope;
      section.setAttribute("aria-labelledby", headingId);
      var heading = document.createElement("h3");
      heading.id = headingId;
      heading.className = "nav-shortcuts__scope";
      heading.textContent = group.heading;
      var dl = document.createElement("dl");
      dl.className = "nav-shortcuts__list";
      rows.forEach(function (shortcut) {
        var row = document.createElement("div");
        row.className = "nav-shortcuts__row";
        var dt = document.createElement("dt");
        dt.textContent = shortcut.description;
        var dd = document.createElement("dd");
        steps(shortcut.keys).forEach(function (step, index) {
          if (index > 0) {
            var then = document.createElement("span");
            then.className = "nav-text-muted";
            then.textContent = "then";
            dd.appendChild(then);
          }
          formatStep(step).forEach(function (cap) {
            var kbd = document.createElement("kbd");
            kbd.className = "nav-kbd";
            kbd.textContent = cap;
            dd.appendChild(kbd);
          });
        });
        row.appendChild(dt);
        row.appendChild(dd);
        dl.appendChild(row);
      });
      section.appendChild(heading);
      section.appendChild(dl);
      list.appendChild(section);
    });
  }

  function dialog() {
    return document.getElementById(DIALOG_ID);
  }

  function isOpen() {
    var el = dialog();
    return !!el && el.hasAttribute("open");
  }

  function openHelp() {
    var el = dialog();
    if (!el || isOpen()) {
      return;
    }
    reset();
    returnFocus = document.activeElement;
    var list = el.querySelector("[data-nav-shortcuts-list]");
    if (list) {
      renderList(list, declarations());
    }
    if (typeof el.showModal === "function") {
      el.showModal();
    } else {
      el.setAttribute("open", "");
    }
  }

  function closeHelp() {
    var el = dialog();
    if (!el || !isOpen()) {
      return;
    }
    if (typeof el.close === "function") {
      el.close();
    } else {
      el.removeAttribute("open");
      restoreFocus();
    }
  }

  function focusBack(target) {
    if (target && target.isConnected && typeof target.focus === "function") {
      target.focus();
    }
  }

  function restoreFocus() {
    var target = returnFocus;
    returnFocus = null;
    focusBack(target);
  }

  /* ----------------------------------------------------------- palette -- */

  function palette() {
    return document.getElementById(PALETTE_ID);
  }

  function paletteIsOpen() {
    var el = palette();
    return !!el && el.hasAttribute("open");
  }

  function paletteList(el) {
    return el.querySelector("[" + PALETTE_RESULTS + "]");
  }

  // The rows a palette shows right now: every `option` the server rendered
  // that the current filter has not hidden.
  function paletteRows(el) {
    var list = paletteList(el);
    if (!list) {
      return [];
    }
    var options = list.querySelectorAll("[role=option]");
    var rows = [];
    for (var i = 0; i < options.length; i += 1) {
      if (!options[i].hasAttribute("hidden")) {
        rows.push(options[i]);
      }
    }
    return rows;
  }

  function selectPaletteRow(row) {
    if (selectedRow) {
      selectedRow.setAttribute("aria-selected", "false");
    }
    selectedRow = row && row.hasAttribute("data-href") ? row : null;
    if (selectedRow) {
      selectedRow.setAttribute("aria-selected", "true");
      if (typeof selectedRow.scrollIntoView === "function") {
        selectedRow.scrollIntoView({ block: "nearest" });
      }
    }
  }

  function movePaletteSelection(step) {
    var el = palette();
    if (!el) {
      return;
    }
    var rows = paletteRows(el);
    if (rows.length === 0) {
      return;
    }
    var at = rows.indexOf(selectedRow);
    var next = Math.min(Math.max(at + step, 0), rows.length - 1);
    selectPaletteRow(rows[next]);
  }

  function choosePaletteSelection() {
    if (!selectedRow) {
      return;
    }
    var href = selectedRow.getAttribute("data-href");
    closePalette();
    if (!href) {
      return;
    }
    // Same-origin only: a tampered data-href must never become a
    // `javascript:` navigation. The shortcut jump applies the same guard.
    var target = new URL(href, window.location.href);
    if (target.origin === window.location.origin) {
      window.location.assign(target.href);
    }
  }

  // Narrow the rows to the ones matching the filter. The set the server
  // scoped never changes — rows only hide; a filter that matches nothing
  // says so instead of leaving a silently empty list.
  function filterPalette(query) {
    var el = palette();
    if (!el) {
      return;
    }
    var list = paletteList(el);
    if (!list) {
      return;
    }
    var needle = (query || "").toLowerCase();
    var options = list.querySelectorAll("[role=option]");
    var first = null;
    var visible = 0;
    for (var i = 0; i < options.length; i += 1) {
      var row = options[i];
      if (row.textContent.toLowerCase().indexOf(needle) !== -1) {
        row.removeAttribute("hidden");
        visible += 1;
        if (!first) {
          first = row;
        }
      } else {
        row.setAttribute("hidden", "hidden");
      }
    }
    var empty = list.querySelector("[data-nav-palette-empty]");
    if (!empty) {
      empty = document.createElement("li");
      empty.className = "nav-palette-dialog__empty";
      empty.setAttribute("data-nav-palette-empty", "true");
      empty.setAttribute("aria-live", "polite");
      empty.textContent = "No matching destinations";
      list.appendChild(empty);
    }
    if (visible === 0) {
      empty.removeAttribute("hidden");
    } else {
      empty.setAttribute("hidden", "hidden");
    }
    selectPaletteRow(visible > 0 ? first : null);
  }

  // A palette opens unfiltered: every destination for the page's role is
  // visible, the first one is pre-selected, and the filter is focused.
  function resetPalette(el) {
    var field = el.querySelector("[" + PALETTE_FILTER + "]");
    if (field) {
      field.value = "";
    }
    var list = paletteList(el);
    if (!list) {
      return;
    }
    var options = list.querySelectorAll("[role=option]");
    for (var i = 0; i < options.length; i += 1) {
      options[i].removeAttribute("hidden");
    }
    var empty = list.querySelector("[data-nav-palette-empty]");
    if (empty) {
      empty.setAttribute("hidden", "hidden");
    }
    selectPaletteRow(options.length > 0 ? options[0] : null);
  }

  function openPalette() {
    var el = palette();
    if (!el || paletteIsOpen()) {
      return;
    }
    // The same snapshot `openHelp()` takes: where focus goes when the
    // palette closes. A half-typed chord lapses with the palette up.
    reset();
    paletteReturnFocus = document.activeElement;
    resetPalette(el);
    if (typeof el.showModal === "function") {
      el.showModal();
    } else {
      el.setAttribute("open", "");
    }
    var field = el.querySelector("[" + PALETTE_FILTER + "]");
    if (field && typeof field.focus === "function") {
      field.focus();
    }
  }

  function closePalette() {
    var el = palette();
    if (!el || !paletteIsOpen()) {
      return;
    }
    if (typeof el.close === "function") {
      el.close();
    } else {
      el.removeAttribute("open");
      restorePaletteFocus();
    }
  }

  function restorePaletteFocus() {
    var target = paletteReturnFocus;
    paletteReturnFocus = null;
    focusBack(target);
  }

  /* ----------------------------------------------------------- dispatch -- */

  document.addEventListener("keydown", function (event) {
    if (paletteIsOpen()) {
      // The palette owns the keyboard while it is up: Escape closes it,
      // the arrows move the selection, Enter jumps, and every other key
      // reaches only the filter field. The help overlay stays shut.
      if (event.key === "Escape") {
        closePalette();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        movePaletteSelection(1);
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        movePaletteSelection(-1);
      } else if (event.key === "Enter") {
        event.preventDefault();
        choosePaletteSelection();
      }
      return;
    }
    if (isOpen()) {
      // The dialog is modal: while it is up it owns the keyboard, and Esc
      // closes it even where the platform's own cancel event never arrives.
      if (event.key === "Escape") {
        closeHelp();
      }
      return;
    }
    if (event.defaultPrevented || MODIFIER_KEYS[event.key]) {
      return;
    }
    var editable = isEditable(event.target);
    var all = declarations().reverse();

    var matched = 0;
    var hits = [];
    if (pending) {
      hits = advance(pending.candidates, pending.matched, event, editable);
      matched = pending.matched;
      reset();
    }
    if (hits.length === 0) {
      matched = 0;
      hits = advance(all, 0, event, editable);
    }
    if (hits.length === 0) {
      return;
    }

    event.preventDefault();
    var done = null;
    for (var i = 0; i < hits.length; i += 1) {
      if (steps(hits[i].keys).length === matched + 1) {
        done = hits[i];
        break;
      }
    }
    if (done) {
      run(done, event);
      return;
    }
    pending = { candidates: hits, matched: matched + 1 };
    timer = window.setTimeout(reset, chordTimeout());
  });

  document.addEventListener("click", function (event) {
    var target = event.target;
    if (target && typeof target.closest === "function") {
      if (target.closest("[data-nav-shortcuts-close]")) {
        closeHelp();
      }
      if (target.closest("[" + PALETTE_CLOSE + "]")) {
        closePalette();
      }
    }
  });

  document.addEventListener(
    "close",
    function (event) {
      if (event.target && event.target.id === DIALOG_ID) {
        restoreFocus();
      }
      if (event.target && event.target.id === PALETTE_ID) {
        restorePaletteFocus();
      }
    },
    true
  );

  // Typing in the palette's filter narrows the list; the delegated listener
  // keeps the CSP clean of inline handlers.
  document.addEventListener("input", function (event) {
    var target = event.target;
    if (
      target &&
      typeof target.hasAttribute === "function" &&
      target.hasAttribute(PALETTE_FILTER)
    ) {
      filterPalette(target.value);
    }
  });

  // A question that has just loaded puts the cursor in its first input, so the
  // next keystroke answers it.
  function focusFirstInput() {
    var input = firstInput(questionForm());
    if (!input) {
      input = document.querySelector("[data-nav-question-title]");
      if (input && !input.hasAttribute("tabindex")) input.setAttribute("tabindex", "-1");
    }
    if (input && (document.activeElement === document.body || !document.activeElement)) {
      input.focus();
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", focusFirstInput);
  } else {
    focusFirstInput();
  }
})();
