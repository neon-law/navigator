// The lawyer's Project workbench. Source text and proposal fields are always rendered as text.
(() => {
  "use strict";
  const el = (name, text, className) => {
    const node = document.createElement(name);
    if (text !== undefined) node.textContent = text;
    if (className) node.className = className;
    return node;
  };
  const button = (label, click, primary = false) => {
    const b = el("button", label, "nav-btn nav-btn--" + (primary ? "primary" : "secondary"));
    b.type = "button";
    b.addEventListener("click", click);
    return b;
  };
  const roles = { signed_order: "Signed order", trigger: "Trigger", authority: "Governing authority" };
  const required = (row) =>
    row.kind === "signed_order"
      ? ["signed_order"]
      : row.kind === "calculated"
        ? ["trigger", "authority"]
        : [];
  function formatted(row) {
    try {
      const dateOnly = typeof row.due_on === "string";
      const date = new Date(dateOnly ? row.due_on + "T12:00:00Z" : row.due_at || "");
      const label = new Intl.DateTimeFormat(undefined, {
        year: "numeric",
        month: "long",
        day: "numeric",
        timeZone: dateOnly ? "UTC" : row.timezone,
      }).format(date);
      const time = (at) =>
        new Intl.DateTimeFormat(undefined, {
          hour: "numeric",
          minute: "2-digit",
          timeZone: row.timezone,
        }).format(at);
      let detail = dateOnly ? "All day" : time(date);
      if (dateOnly && row.cutoff) detail += " · Cutoff " + row.cutoff;
      if (!dateOnly && (row.category === "hearing" || row.calendar_role === "hearing")) {
        const minutes = row.hearing?.duration_minutes || 60;
        detail += "–" + time(new Date(date.getTime() + minutes * 60000));
      }
      // Validate the supplied IANA zone even for an all-day date.
      time(date);
      return {
        label,
        detail: [detail, row.timezone, row.hearing?.location].filter(Boolean).join(" · "),
      };
    } catch {
      return { label: "Date unavailable", detail: row.timezone || "", invalid: true };
    }
  }
  class CourtDateReview extends HTMLElement {
    connectedCallback() {
      if (this.started) return;
      this.started = true;
      this.queue = this.querySelector("[data-queue]");
      this.detail = this.querySelector("[data-detail]");
      this.message = this.querySelector("[data-message]");
      this.dialog = this.querySelector("dialog");
      this.count = this.querySelector(".nav-status-strip__value");
      this.rows = [];
      this.filter = "pending";
      this.sources = new Map();
      this.dialog.addEventListener("close", () => {
        if (this.opener?.isConnected) this.opener.focus();
        else this.focusRow();
      });
      this.addEventListener("review-shortcut", (e) => {
        if (this.querySelector("dialog[open]") || this.busy || !this.row) return;
        switch (e.detail) {
          case "j":
            this.move(1);
            break;
          case "k":
            this.move(-1);
            break;
          case "r":
            this.sources.get(this.row.kind === "signed_order" ? "signed_order" : "trigger")?.openPage();
            break;
          case "a":
            this.sources.get("authority")?.openPage();
            break;
          case "Mod+Enter":
            if (!this.confirm.disabled) this.confirm.click();
            break;
        }
      });
      this.load();
    }
    async load(preferred) {
      this.busy = true;
      try {
        const response = await fetch(this.dataset.endpoint, {
          credentials: "same-origin",
          cache: "no-store",
        });
        if (!response.ok)
          throw new Error("The review queue could not be read (" + response.status + ").");
        const data = await response.json();
        if (!Array.isArray(data.proposals)) throw new Error("The review queue could not be read.");
        this.proposals = data.proposals;
        this.canConfirm = this.dataset.supervised !== "true" && data.can_confirm === true;
        this.busy = false;
        if (!this.canConfirm && !preferred && data.proposals.some((p) => p.status === "returned"))
          this.filter = "returned";
        this.showQueue(preferred);
      } catch (err) {
        this.busy = false;
        this.row = null;
        this.canConfirm = false;
        this.generation++;
        this.querySelector("[data-queue-views]").replaceChildren();
        this.queue.replaceChildren();
        this.count.textContent = "—";
        this.detail.replaceChildren(
          el("p", err.message),
          button("Retry", () => this.load(preferred)),
        );
      }
    }
    showQueue(preferred) {
      const views = this.querySelector("[data-queue-views]");
      views.replaceChildren();
      if (this.proposals.some((p) => p.status === "returned")) {
        for (const status of ["pending", "returned"]) {
          const count = this.proposals.filter((p) => p.status === status).length;
          const tab = button(
            (status === "pending" ? "Pending" : "Returned") + " (" + count + ")",
            () => {
              if (this.busy) return;
              this.filter = status;
              this.showQueue();
              this.focusRow();
            },
          );
          tab.setAttribute("aria-pressed", String(status === this.filter));
          views.append(tab);
        }
      } else this.filter = "pending";
      this.count.textContent = String(this.proposals.filter((p) => p.status === "pending").length);
      this.rows = this.proposals.filter((p) => p.status === this.filter);
      this.cells = new Map();
      this.queue.replaceChildren();
      for (const item of this.rows) {
        const b = button("", () => {
          if (!this.busy) {
            this.select(item);
            this.focusRow();
          }
        });
        b.className = "court-review__row";
        this.cells.set(item.id, b);
        b.append(
          el("small", formatted(item).label + " · " + this.kind(item)),
          el("strong", item.title),
        );
        this.queue.append(b);
      }
      this.select(this.rows.find((p) => p.id === preferred) || this.rows[0]);
    }
    focusRow() {
      this.queue.querySelector('[aria-current="true"]')?.focus();
    }
    move(step) {
      if (!this.rows.length || this.busy) return;
      const index = (this.rows.indexOf(this.row) + step + this.rows.length) % this.rows.length;
      this.select(this.rows[index]);
      this.focusRow();
    }
    select(row) {
      this.cells.get(this.row?.id)?.removeAttribute("aria-current");
      this.cells.get(row?.id)?.setAttribute("aria-current", "true");
      this.row = row;
      this.generation = (this.generation || 0) + 1;
      this.sources.clear();
      this.detail.replaceChildren();
      if (!row) {
        this.detail.append(el("p", "No " + this.filter + " court dates."));
        return;
      }
      const when = formatted(row),
        expected = required(row);
      const evidence = Array.isArray(row.evidence) ? row.evidence : [];
      this.detail.append(
        el("small", this.kind(row)),
        el("h3", when.label, "court-review__date"),
        el("p", row.title),
        el("p", when.detail, "nav-muted"),
      );
      const hearing = row.hearing;
      if (hearing?.appearance?.kind === "trial_day") {
        const starts = [row.due_at, ...(hearing.appearance.additional_starts || [])];
        const list = el("ol");
        for (const start of starts) list.append(el("li", formatted({ ...row, due_at: start }).label));
        this.detail.append(
          el("h4", hearing.appearance.cancel ? "Cancel this trial day" : "Trial dates"),
          list,
          el("p", hearing.duration_minutes + " minutes · " + hearing.location),
        );
      }
      if (hearing?.prep?.length) {
        const list = el("ul");
        for (const entry of hearing.prep) {
          list.append(el("li", entry.title + " · " + entry.due_at + " · " + entry.effort_minutes + " minutes"));
        }
        this.detail.append(el("h4", "Preparation deadlines"), list);
      }
      let loaded = 0;
      const supported =
        !row.review_error &&
        row.title?.trim() &&
        Number.isSafeInteger(row.revision) &&
        (row.kind !== "calculated" || row.calculation?.trigger?.trim()) &&
        expected.length > 0 &&
        expected.every((role) => evidence.some((e) => e.role === role));
      if (row.status === "returned") {
        this.detail.append(el("h4", "Returned for correction"), el("p", row.reason));
        const code = this.dataset.projectCode;
        const projectFlag =
          code && /^[a-zA-Z0-9][a-zA-Z0-9_-]*$/.test(code) ? " --project " + code : "";
        const command = el(
          "code",
          "navigator project deadline review correct " +
            row.id +
            " --revision " +
            row.revision +
            " --file draft.json" +
            projectFlag,
        );
        this.detail.append(command);
      }
      const calc = row.calculation;
      if (calc) {
        const sign = calc.offset_days < 0 ? "−" : "+";
        const basis = String(calc.counting_basis).replaceAll("_", " ");
        this.detail.append(
          el(
            "p",
            calc.trigger +
              " · " +
              calc.trigger_on +
              " " +
              sign +
              " " +
              Math.abs(calc.offset_days) +
              " " +
              basis +
              " = " +
              when.label,
          ),
        );
      }
      const actions = el("div", undefined, "court-review__actions");
      const confirmation =
        hearing?.appearance?.cancel
          ? "Confirm trial day cancellation"
          : hearing?.appearance?.kind === "trial_day"
            ? "Confirm trial dates and preparation"
            : row.kind === "signed_order" ? "Confirm signed order and date" : "Confirm date";
      this.confirm = button(confirmation, () => this.submitDecision("confirm"), true);
      this.confirm.disabled = true;
      const correct = button("Request correction", () => this.correction());
      correct.disabled = !this.canConfirm;
      const showConfirmation = this.canConfirm && row.status === "pending";
      if (showConfirmation) actions.append(this.confirm, correct);
      actions.append(button("Skip", () => this.move(1)));
      const generation = this.generation;
      for (const [index, source] of evidence.entries()) {
        const section = el("section", undefined, "court-review__source");
        this.detail.append(section);
        const role = roles[source.role] || "Source";
        if (!this.canConfirm && source.role === "authority") {
          section.append(el("h4", role), el("p", "Authority page available to the reviewing lawyer."));
          continue;
        }
        const path =
          this.dataset.endpoint +
          "/" +
          encodeURIComponent(row.id) +
          "/evidence/" +
          index +
          "?revision=" +
          encodeURIComponent(row.revision);
        const highlight = document.createElement("nav-citation-highlight");
        const label = source.label?.trim();
        highlight.dataset.label = label ? role + " · " + label : role;
        highlight.dataset.page = source.page;
        highlight.dataset.src = path;
        highlight.dataset.rects = JSON.stringify(source.rects || []);
        if (source.forgejo_url) {
          highlight.dataset.forgejo = source.forgejo_url;
          const pin = source.forgejo;
          const repository = { project: "Matter repository", firm_library: "Firm Library" }[pin?.repository];
          if (repository && pin.path && /^[a-f0-9]{40}$/.test(pin.commit))
            highlight.forgejoTitle = repository + " · " + pin.path + " · " + pin.commit.slice(0, 8);
        }
        highlight.addEventListener("citation-ready", () => {
          if (this.generation !== generation) return;
          if (!this.sources.has(source.role)) this.sources.set(source.role, highlight);
          loaded++;
          this.confirm.disabled =
            this.busy ||
            !this.canConfirm ||
            row.status !== "pending" ||
            !supported ||
            when.invalid === true ||
            loaded !== evidence.length;
        });
        highlight.addEventListener("citation-error", () => {
          if (this.generation === generation) section.append(button("Reload proposal", () => this.load(row.id)));
        });
        section.append(highlight);
      }
      if ((!supported || when.invalid) && row.status === "pending") {
        this.detail.append(
          el(
            "p",
            row.review_error ||
              "This proposal needs a date and supporting evidence before confirmation.",
          ),
        );
      }
      this.detail.append(actions);
      if (showConfirmation) {
        const hint = el(
          "small",
          "Confirmed dates appear on the matter’s shared calendar after the next sync.",
          "court-review__calendar-note nav-muted",
        );
        hint.id = "court-date-calendar-" + row.id;
        this.confirm.setAttribute("aria-describedby", hint.id);
        this.detail.append(hint);
      }
    }
    kind(row) {
      return row.kind === "signed_order" ? "Express date" : "Calculated date";
    }
    open(...children) {
      this.opener = document.activeElement;
      this.dialog.replaceChildren(...children);
      this.dialog.showModal();
    }
    correction() {
      const label = el("label", "What needs correction?");
      const field = el("textarea");
      field.id = "court-date-correction";
      label.htmlFor = field.id;
      const send = button(
        "Return for correction",
        () => this.submitDecision("return", field.value.trim()),
        true,
      );
      send.disabled = true;
      field.addEventListener("input", () => {
        send.disabled = !field.value.trim();
      });
      this.open(
        label,
        field,
        send,
        button("Cancel", () => this.dialog.close()),
      );
      field.focus();
    }
    async submitDecision(action, reason) {
      const row = this.row;
      if (
        !row ||
        row.status !== "pending" ||
        !this.canConfirm ||
        this.busy ||
        (action === "confirm" && this.confirm.disabled)
      )
        return;
      this.busy = true;
      const next = this.rows[this.rows.indexOf(row) + 1]?.id;
      const responseMessage = action === "confirm" ? "Date confirmed." : "Returned for correction.";
      try {
        const response = await fetch(
          this.dataset.endpoint + "/" + encodeURIComponent(row.id) + "/" + action,
          {
            method: "POST",
            redirect: "error",
            credentials: "same-origin",
            cache: "no-store",
            headers: { "Content-Type": "application/json", "X-CSRF-Token": this.dataset.csrf },
            body: JSON.stringify(
              reason ? { revision: row.revision, reason } : { revision: row.revision },
            ),
          },
        );
        if (response.status === 409 || response.status === 404) {
          if (this.dialog.open) this.dialog.close();
          this.message.textContent =
            "This proposal changed. Review the current revision before confirming.";
          await this.load(row.id);
        } else if (response.ok) {
          const saved = await response.json();
          const expected = action === "confirm" ? ["approved", "published"] : ["returned"];
          if (
            saved.id !== row.id ||
            saved.revision !== row.revision ||
            !expected.includes(saved.status)
          ) {
            throw new Error("The server did not verify this decision.");
          }
          if (this.dialog.open) this.dialog.close();
          this.message.textContent = row.title + " · " + responseMessage;
          await this.load(next);
        } else throw new Error("Decision could not be verified (" + response.status + ").");
        this.focusRow();
      } catch (err) {
        this.busy = false;
        this.confirm.disabled = true;
        const status = el("p", err.message + " Reload the proposal before trying again.");
        const retry = button("Reload proposal", () => {
          if (this.dialog.open) this.dialog.close();
          this.load(row.id);
        });
        (this.dialog.open ? this.dialog : this.detail).append(status, retry);
      }
    }
  }
  if (!customElements.get("court-date-review"))
    customElements.define("court-date-review", CourtDateReview);
})();
