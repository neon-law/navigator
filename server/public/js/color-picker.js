// One submitted hex field, with a native picker as progressive enhancement.
document.addEventListener("input", (event) => {
  const control = event.target;
  if (!(control instanceof HTMLInputElement)) return;
  const group = control.closest("[data-color-picker]");
  if (!group) return;
  const text = group.querySelector('input[type="text"]');
  const picker = group.querySelector('input[type="color"]');
  if (control === picker) text.value = picker.value;
  else if (control === text && /^#[0-9a-f]{6}$/i.test(text.value)) {
    picker.value = text.value;
  }
});
