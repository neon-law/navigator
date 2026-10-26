// Saved destinations stay protected until the user explicitly chooses Update.
document.addEventListener("click", (event) => {
  if (!(event.target instanceof Element)) return;
  const button = event.target.closest("button[data-account-update]");
  if (!button) return;
  const input = document.getElementById(button.dataset.accountUpdate);
  if (!input || !button.closest("form").contains(input)) return;
  const form = button.closest("form");
  const editing = button.getAttribute("aria-pressed") !== "true";
  if (!input.dataset.savedValue) input.dataset.savedValue = input.value;
  input.readOnly = !editing;
  form.querySelector("[data-account-save]").hidden = !editing;
  const connect = form.querySelector("[data-account-connect]");
  if (connect) connect.disabled = editing;
  button.setAttribute("aria-pressed", String(editing));
  if (editing) {
    button.dataset.updateLabel = button.textContent;
    button.textContent = "Cancel update";
    input.focus();
  } else {
    input.value = input.dataset.savedValue;
    button.textContent = button.dataset.updateLabel;
  }
});
