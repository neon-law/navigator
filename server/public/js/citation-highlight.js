// A cited passage and its original page, drawn from the same image and normalized rectangles.
(() => {
  "use strict";
  function element(tag, text, className) {
    const node = document.createElement(tag);
    if (text) node.textContent = text;
    if (className) node.className = className;
    return node;
  }
  function localPage(path) {
    if (!path || !path.startsWith("/") || path.startsWith("//")) return null;
    try {
      const url = new URL(path, location.origin);
      return url.origin === location.origin ? url : null;
    } catch {
      return null;
    }
  }
  function sourceLink(label, url, title) {
    const link = element("a", label, "nav-link");
    link.href = url.href;
    link.title = title;
    link.setAttribute("aria-label", label + ": " + title);
    link.target = "_blank";
    link.rel = "noopener";
    return link;
  }
  function validRect(r) {
    return (
      r &&
      [r.x, r.y, r.width, r.height].every(Number.isFinite) &&
      r.x >= 0 &&
      r.y >= 0 &&
      r.width > 0 &&
      r.height > 0 &&
      r.x + r.width <= 1.000001 &&
      r.y + r.height <= 1.000001
    );
  }
  function markedPage(image, rectangles, full) {
    const width = image.naturalWidth,
      height = image.naturalHeight;
    const top = full ? 0 : Math.max(0, Math.min(...rectangles.map((r) => r.y)) - 0.035);
    const bottom = full ? 1 : Math.min(1, Math.max(...rectangles.map((r) => r.y + r.height)) + 0.035);
    const canvas = document.createElement("canvas");
    canvas.setAttribute("aria-hidden", "true");
    canvas.width = width;
    canvas.height = Math.max(1, Math.ceil((bottom - top) * height));
    const ctx = canvas.getContext("2d");
    ctx.drawImage(image, 0, -top * height);
    ctx.fillStyle = "rgba(251,206,24,.32)";
    ctx.strokeStyle = "#946b00";
    ctx.lineWidth = Math.max(1, width / 730);
    for (const r of rectangles) {
      const box = [r.x * width, (r.y - top) * height, r.width * width, r.height * height];
      ctx.fillRect(...box);
      ctx.strokeRect(...box);
    }
    return canvas;
  }
  class CitationHighlight extends HTMLElement {
    connectedCallback() {
      if (this.started) return;
      this.started = true;
      const source = localPage(this.dataset.src);
      const page = Number(this.dataset.page);
      const title = this.dataset.label + ", page " + page;
      const heading = element("h4", title, "nav-citation-highlight__heading");
      this.replaceChildren(heading);
      let rects;
      try {
        rects = JSON.parse(this.dataset.rects);
      } catch {
        rects = [];
      }
      if (
        !source || !Number.isSafeInteger(page) || page < 1 || !Array.isArray(rects) ||
        !rects.length || !rects.every(validRect)
      ) {
        this.append(element("p", "A verified page and highlight are required."));
        return;
      }
      const preview = element("button", "Loading source page…", "nav-citation-highlight__preview");
      preview.type = "button";
      preview.disabled = true;
      preview.setAttribute("aria-label", "Open full page: " + title);
      preview.setAttribute("aria-haspopup", "dialog");
      this.append(preview);
      // A provenance link is valid only for this same pinned evidence page and revision.
      const forgejo = localPage(this.dataset.forgejo);
      const expected = new URL(source);
      expected.pathname += "/forgejo";
      const hasProvenance = /^\/app\/api\/(?:[^/]+\/)+evidence\/[^/]+$/.test(source.pathname)
        && /^\?revision=\d+$/.test(source.search) && forgejo?.href === expected.href;
      const repositoryTitle = this.forgejoTitle || this.dataset.forgejoTitle || title;
      if (hasProvenance) this.append(sourceLink("Open in Forgejo", forgejo, repositoryTitle));
      const image = new Image();
      image.onload = () => {
        preview.replaceChildren(markedPage(image, rects, false));
        preview.disabled = false;
        this.dispatchEvent(new Event("citation-ready"));
      };
      image.onerror = () => {
        preview.textContent = "Source unavailable. Reload to check for a newer revision.";
        this.dispatchEvent(new Event("citation-error"));
      };
      this.openPage = () => {
        if (preview.disabled || !this.isConnected || this.querySelector("dialog[open]")) return;
        const dialog = element("dialog");
        dialog.setAttribute("aria-label", title);
        const full = markedPage(image, rects, true);
        full.setAttribute("role", "img");
        full.removeAttribute("aria-hidden");
        full.setAttribute("aria-label", "Original page with highlighted passages: " + title);
        const close = element("button", "Close", "nav-btn nav-btn--secondary");
        close.type = "button";
        close.addEventListener("click", () => dialog.close());
        dialog.append(element("h3", title), sourceLink("Open original page image", source, title));
        if (hasProvenance) dialog.append(sourceLink("Open in Forgejo", forgejo, repositoryTitle));
        dialog.append(full, close);
        dialog.addEventListener("close", () => {
          dialog.remove();
          preview.focus();
        }, { once: true });
        this.append(dialog);
        dialog.showModal();
      };
      preview.addEventListener("click", this.openPage);
      image.src = source.href;
    }
  }
  if (!customElements.get("nav-citation-highlight"))
    customElements.define("nav-citation-highlight", CitationHighlight);
})();
