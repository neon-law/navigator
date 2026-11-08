(() => {
  const search = document.querySelector("#template-search");
  const results = document.querySelector("#template-results");
  const status = document.querySelector("#template-search-status");
  if (!search || !results || !status) return;

  const filters = results.closest(".template-gallery-wrapper");
  const normalize = (text) => text.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
  const groups = Array.from(results.querySelectorAll(".template-gallery__group"), (group) => ({
    group,
    cards: Array.from(group.querySelectorAll("[data-template-search]"), (card) => ({
      card,
      text: normalize(card.dataset.templateSearch).join(" "),
    })),
  }));

  function filterTemplates() {
    const terms = normalize(search.value);
    const kind = filters.querySelector("input:checked").id.slice("kind-filter-".length);
    let count = 0;
    for (const { group, cards } of groups) {
      let visible = 0;
      for (const { card, text } of cards) {
        card.hidden = !terms.every((term) => text.includes(term));
        if (!card.hidden && (kind === "all" || card.dataset.kind === kind)) visible++;
      }
      group.hidden = visible === 0;
      count += visible;
    }
    status.textContent = count === 0 ? "No matching templates." :
      `${count} ${count === 1 ? "template" : "templates"}`;
  }

  search.addEventListener("input", filterTemplates);
  filters.addEventListener("change", filterTemplates);
  search.closest(".template-search").hidden = false;
  filterTemplates();
})();
