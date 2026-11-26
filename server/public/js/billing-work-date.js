// Load rates and entries for the calendar day on the lawyer's device.
(() => {
  const url = new URL(window.location.href);
  if (url.searchParams.has("day")) return;

  const now = new Date();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  url.searchParams.set("day", `${now.getFullYear()}-${month}-${day}`);
  window.location.replace(url.href);
})();
