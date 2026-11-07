class SearchEngine {
  constructor() {
    this.index = [];
    this.modal = document.getElementById("search-modal");
    this.input = document.getElementById("search-input");
    this.results = document.getElementById("search-results");
    this.trigger = document.getElementById("search-trigger");
    this.selectedIndex = -1;
    // Where the keyboard was before the search opened, so closing puts it back.
    this.opener = null;

    this.bindEvents();
  }

  bindEvents() {
    this.trigger.addEventListener("click", () => this.open());

    document.addEventListener("keydown", (e) => {
      const isOpen = this.modal.classList.contains("active");
      if ((e.ctrlKey || e.metaKey) && e.key === "k") {
        e.preventDefault();
        this.open();
      }
      // `/` opens it too, as the sidebar's label says, unless it is being typed somewhere.
      if (e.key === "/" && !isOpen && !e.ctrlKey && !e.metaKey && !e.altKey && !typing(e.target)) {
        e.preventDefault();
        this.open();
      }
      if (e.key === "Escape" && isOpen) {
        e.preventDefault();
        this.close();
      }
      // The dialog holds the keyboard: its one control is the field, so Tab stays there.
      if (e.key === "Tab" && isOpen) {
        e.preventDefault();
        this.input.focus();
      }
    });

    this.modal.addEventListener("click", (e) => {
      if (e.target === this.modal) this.close();
    });

    this.input.addEventListener("input", () => {
      this.selectedIndex = -1;
      this.search(this.input.value);
    });

    this.input.addEventListener("keydown", (e) => {
      const items = this.results.querySelectorAll(".search-result-item");
      if (e.key === "ArrowDown") {
        e.preventDefault();
        this.selectedIndex = Math.min(
          this.selectedIndex + 1,
          items.length - 1
        );
        this.updateSelection(items);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        this.selectedIndex = Math.max(this.selectedIndex - 1, items.length ? 0 : -1);
        this.updateSelection(items);
      } else if (e.key === "Enter" && this.selectedIndex >= 0) {
        e.preventDefault();
        items[this.selectedIndex]?.click();
      }
    });
  }

  updateSelection(items) {
    items.forEach((item, i) => {
      const on = i === this.selectedIndex;
      item.classList.toggle("selected", on);
      item.setAttribute("aria-selected", on);
    });
    const current = items[this.selectedIndex];
    if (current) {
      this.input.setAttribute("aria-activedescendant", current.id);
      current.scrollIntoView({ block: "nearest" });
    } else {
      this.input.removeAttribute("aria-activedescendant");
    }
  }

  open() {
    if (this.modal.classList.contains("active")) return;
    this.opener = document.activeElement;
    this.modal.classList.add("active");
    this.clear();
    setTimeout(() => this.input.focus(), 50);
  }

  close() {
    this.modal.classList.remove("active");
    this.clear();
    this.opener?.focus?.();
    this.opener = null;
  }

  clear() {
    this.input.value = "";
    this.results.innerHTML = "";
    this.selectedIndex = -1;
    this.input.removeAttribute("aria-activedescendant");
    this.input.setAttribute("aria-expanded", "false");
  }

  buildIndex(pages) {
    this.index = pages.map((page) => ({
      title: page.title,
      slug: page.slug,
      content: page.content.toLowerCase(),
      rawContent: page.content,
    }));
  }

  search(query) {
    this.input.removeAttribute("aria-activedescendant");
    if (!query.trim()) {
      this.results.innerHTML = "";
      this.input.setAttribute("aria-expanded", "false");
      return;
    }

    const terms = query.toLowerCase().trim().split(/\s+/);
    const matches = [];

    for (const page of this.index) {
      const allMatch = terms.every((term) => page.content.includes(term));
      if (!allMatch) continue;

      const snippet = this.getSnippet(page.rawContent, terms[0]);
      matches.push({
        title: page.title,
        slug: page.slug,
        snippet,
      });
    }

    this.renderResults(matches, terms);
  }

  getSnippet(content, term) {
    const lower = content.toLowerCase();
    const idx = lower.indexOf(term.toLowerCase());
    if (idx === -1) return content.slice(0, 120) + "...";

    const start = Math.max(0, idx - 50);
    const end = Math.min(content.length, idx + 80);
    let snippet = content.slice(start, end);

    if (start > 0) snippet = "..." + snippet;
    if (end < content.length) snippet += "...";

    return snippet;
  }

  // The text escaped, with every term marked; split first so a term never lands in an entity.
  highlightTerms(text, terms) {
    const pattern = terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|");
    return text
      .split(new RegExp(`(${pattern})`, "gi"))
      .map((part, i) => (i % 2 ? `<mark>${esc(part)}</mark>` : esc(part)))
      .join("");
  }

  renderResults(matches, terms) {
    if (matches.length === 0) {
      this.results.innerHTML =
        '<div class="search-empty" role="status">No results found.</div>';
      this.input.setAttribute("aria-expanded", "false");
      return;
    }

    this.input.setAttribute("aria-expanded", "true");
    this.results.innerHTML = matches
      .map(
        (match, i) => `
      <div class="search-result-item" id="search-option-${i}" role="option" aria-selected="false" data-slug="${esc(match.slug)}">
        <div class="search-result-title">${this.highlightTerms(match.title, terms)}</div>
        <div class="search-result-snippet">${this.highlightTerms(match.snippet, terms)}</div>
      </div>
    `
      )
      .join("");

    this.results.querySelectorAll(".search-result-item").forEach((item) => {
      item.addEventListener("click", () => this.pick(item.dataset.slug));
    });
  }

  // A picked result moves the keyboard to the page it opens, not back to the opener.
  pick(slug) {
    this.opener = null;
    this.close();
    const focusHeading = () => {
      const h = [...document.querySelectorAll("#home h1, #article h1")].find((el) => el.offsetParent);
      if (!h) return;
      h.tabIndex = -1;
      h.focus();
    };
    if (window.location.hash.slice(1) === slug) {
      focusHeading();
      return;
    }
    document.addEventListener("docs:page", focusHeading, { once: true });
    window.location.hash = slug;
  }
}

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

// A key pressed in a field types there; it is not a shortcut.
const typing = (el) => el?.closest?.("input, textarea, select, [contenteditable='true']");

const searchEngine = new SearchEngine();
export { searchEngine };
