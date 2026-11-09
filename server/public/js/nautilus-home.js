(() => {
  const form = document.querySelector("#nautilus-search");
  const question = document.querySelector("#nautilus-question");
  const dialog = document.querySelector("#nautilus-search-dialog");
  if (!form || !question || !dialog) return;

  const key = "nautilus-pending-question";
  const signedIn = dialog.dataset.signedIn === "true";
  const video = dialog.querySelector("video");
  let checkedVideo = false;

  function openDialog() {
    dialog.showModal();
    if (checkedVideo) return;
    checkedVideo = true;
    fetch(video.dataset.source, { method: "HEAD" }).then((response) => {
      if (response.ok && response.headers.get("content-length") !== "0"
          && response.headers.get("content-type")?.startsWith("video/")) {
        video.src = video.dataset.source;
      }
    }).catch(() => {});
  }

  for (const suggestion of form.querySelectorAll("[data-question]")) {
    suggestion.addEventListener("click", () => {
      question.value = suggestion.dataset.question;
      question.focus();
    });
  }

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (question.value.trim()) openDialog();
  });

  dialog.addEventListener("close", () => {
    video.pause();
    question.focus();
  });

  for (const link of dialog.querySelectorAll('a[href^="/auth/login/"]')) {
    link.addEventListener("click", () => {
      try {
        sessionStorage.setItem(key, JSON.stringify({
          question: question.value.trim(),
          expires: Date.now() + 15 * 60 * 1000,
        }));
      } catch {
        // Sign-in can still proceed when browser storage is unavailable.
      }
    });
  }

  try {
    const pending = JSON.parse(sessionStorage.getItem(key));
    sessionStorage.removeItem(key);
    if (pending?.expires > Date.now() && typeof pending.question === "string") {
      question.value = pending.question;
      if (signedIn) openDialog();
    }
  } catch {
    // Storage is optional. The homepage still accepts a new question.
  }
})();
