// Only the four intake answers can be saved. Contact details and consent stay in the form.
(function () {
  "use strict";
  const key = "iep-intake-draft-v1";
  const fields = ["district", "relationship", "concern", "next_date"];
  function init() {
    let storage;
    try { storage = window.localStorage; } catch { return; }
    const pending = key + "-submitted";
    if (document.getElementById("signup")) {
      try {
        const revision = sessionStorage.getItem(pending);
        sessionStorage.removeItem(pending);
        if (location.hash === "#signup" && revision && JSON.parse(storage.getItem(key))?.revision === revision) {
          storage.removeItem(key);
        }
      } catch { /* Storage may be disabled. */ }
      return;
    }
    const controls = document.getElementById("iep-draft-controls");
    if (!controls) return;
    const form = controls.closest("form");
    const save = document.getElementById("iep-save-draft");
    const status = document.getElementById("iep-draft-status");
    try {
      const draft = JSON.parse(storage.getItem(key));
      if (draft && draft.expires > Date.now() && draft.answers) {
        for (const name of fields) form.elements[name].value = String(draft.answers[name] || "");
        save.checked = true;
        status.textContent = "Draft restored.";
      } else {
        storage.removeItem(key);
      }
    } catch { /* The ordinary form also works without a draft. */ }
    controls.hidden = false;
    function update() {
      try {
        if (save.checked) {
          const answers = Object.fromEntries(fields.map(name => [name, form.elements[name].value]));
          storage.setItem(key, JSON.stringify({ revision: crypto.randomUUID(), expires: Date.now() + 86400000, answers }));
          status.textContent = "Draft saved on this device.";
        } else {
          storage.removeItem(key);
          status.textContent = "Device copy removed.";
        }
      } catch { status.textContent = "This browser could not update the saved draft."; }
    }
    form.addEventListener("submit", () => {
      try {
        sessionStorage.removeItem(pending);
        const draft = JSON.parse(storage.getItem(key));
        if (save.checked && draft?.revision && fields.every(name => draft.answers[name] === form.elements[name].value)) {
          sessionStorage.setItem(pending, draft.revision);
        }
      } catch { /* Submission also works without browser storage. */ }
    });
    save.addEventListener("change", update);
    form.addEventListener("input", event => {
      if (save.checked && fields.includes(event.target.name)) update();
    });
    document.getElementById("iep-clear-draft").addEventListener("click", () => {
      for (const name of fields) form.elements[name].value = "";
      save.checked = false;
      update();
    });
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
