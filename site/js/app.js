import { searchEngine } from "./search.js";
import config from "../site.config.js";

const features = Object.assign(
  { codeCopyButtons: true, readingProgress: true, skeletonLoading: true },
  config.features || {}
);

// The sidebar's sections are drawn as qoop's own spaces, each in its space colour from
// docs/brand.md (violet, lime, cyan, pink, sky, yellow in wheel order).
const SPACE_COUNT = 6;

// ========== Small shared pieces ==========
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const ICONS = {
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h8"/>',
  check: '<path d="m5 12 5 5 9-10"/>',
  chev: '<path d="m9 6 6 6-6 6"/>',
  browser: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9M12 3c-2.5 2.6-3.8 5.6-3.8 9s1.3 6.4 3.8 9"/>',
  apple: '<path d="M12 7.5c-1.4-1-4.2-1.2-5.6.9-1.5 2.2-.9 6.2 1.2 9 .9 1.3 1.9 2 2.8 1.7.6-.2 1-.5 1.6-.5s1 .3 1.6.5c.9.3 1.9-.4 2.8-1.7 2.1-2.8 2.7-6.8 1.2-9-1.4-2.1-4.2-1.9-5.6-.9z"/><path d="M12 7.5c0-2 1-3.4 2.8-4"/>',
  play: '<path d="M6.5 4.2v15.6a.6.6 0 0 0 .9.5l12.6-7.8a.6.6 0 0 0 0-1L7.4 3.7a.6.6 0 0 0-.9.5z"/>',
  download: '<path d="M12 4v11M7 10.5l5 5 5-5M5 20h14"/>',
};
const icon = (name) => `<svg class="i" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`;

// A state picture on the nine-dot grid of docs/brand.md; the dots that are off stay as a
// faint ghost of the grid.
const MARKS = {
  done: [1, 1, 1, 1, 1, 1, 1, 1, 1],
  ring: [1, 1, 1, 1, 0, 1, 1, 1, 1],
  idle: [0, 0, 0, 0, 1, 0, 0, 0, 0],
  line: [0, 0, 0, 1, 1, 1, 0, 0, 0],
  work: [0, 1, 0, 0, 1, 1, 0, 0, 0],
};
const SECTION_MARKS = ["ring", "done", "work", "line"];
function mark(name, size = 12) {
  const dots = MARKS[name]
    .map((on, k) => `<circle cx="${2 + (k % 3) * 4}" cy="${2 + Math.floor(k / 3) * 4}" r="1.45" fill="currentColor"${on ? "" : ' opacity=".16"'}/>`)
    .join("");
  return `<svg class="mark" width="${size}" height="${size}" viewBox="0 0 12 12" aria-hidden="true">${dots}</svg>`;
}

// Inline markdown for a table cell: code spans, bold and links.
function inlineMarkdown(s) {
  const codes = [];
  s = s.replace(/`([^`]+)`/g, (_, c) => {
    codes.push(c);
    return `\x00${codes.length - 1}\x00`;
  });
  s = esc(s)
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');
  return s.replace(/\x00(\d+)\x00/g, (_, i) => `<code>${esc(codes[i])}</code>`);
}

function copyButton(text) {
  return `<button class="copy-btn" data-copy="${esc(text)}">${icon("copy")}<span>Copy</span></button>`;
}

// What a screen reader hears after an action with no other visible outcome, such as Copy.
function announce(text) {
  const status = document.getElementById("status");
  status.textContent = "";
  setTimeout(() => (status.textContent = text), 50);
}

document.addEventListener("click", async (e) => {
  const btn = e.target.closest("[data-copy]");
  if (!btn) return;
  const label = btn.querySelector("span");
  try {
    await navigator.clipboard.writeText(btn.dataset.copy);
    btn.classList.add("copied");
    label.textContent = "Copied";
    announce("Copied");
  } catch {
    label.textContent = "Copy failed";
    announce("Copy failed: select the text and copy it");
  }
  setTimeout(() => {
    btn.classList.remove("copied");
    label.textContent = "Copy";
  }, 2000);
});

// ========== Render Navbar ==========
function renderNavbar() {
  document.title = config.description
    ? `${config.name} · ${config.description}`
    : config.name;

  const navLinks = document.querySelector(".navbar-links");
  navLinks.innerHTML = config.navLinks
    .map((link) => {
      const external = link.external
        ? ' target="_blank" rel="noopener"'
        : "";
      return `<a href="${link.href}"${external}>${link.label}</a>`;
    })
    .join("");

  const footer = document.querySelector(".footer");
  footer.innerHTML = `
    <span>${config.name} · ${config.footer?.left || ""}</span>
    ${config.author?.url ? `<a href="${config.author.url}" target="_blank" rel="noopener">GitHub</a>` : ""}
  `;
}

// ========== Minimal Markdown Parser ==========
function parseMarkdown(md) {
  let html = md;

  // Extract code blocks and inline code FIRST to protect from further parsing
  const codeBlocks = [];
  const inlineCodes = [];

  // Code blocks (fenced) · extract and replace with placeholders
  html = html.replace(
    /```(\w*)\n([\s\S]*?)```/g,
    (_, lang, code) => {
      const escaped = esc(code.trim());
      const langAttr = lang ? ` class="language-${lang}"` : "";
      const dataLang = lang ? ` data-lang="${lang}"` : "";
      const placeholder = `\n\n\x00CODEBLOCK${codeBlocks.length}\x00\n\n`;
      codeBlocks.push(`<pre${dataLang}><code${langAttr}>${escaped}</code></pre>`);
      return placeholder;
    }
  );

  // Inline code · extract and replace with placeholders
  html = html.replace(/`([^`]+)`/g, (_, code) => {
    const placeholder = `\x00INLINECODE${inlineCodes.length}\x00`;
    inlineCodes.push(`<code>${esc(code)}</code>`);
    return placeholder;
  });

  // The docs are plain markdown, never HTML: a placeholder such as <agent> in prose is text.
  // Everything below turns markdown into tags on top of this escaped text.
  html = esc(html);

  // Headings. Every block below stands between blank lines, so the paragraph pass never
  // leaves a line of text glued to one.
  html = html.replace(/^#### (.+)$/gm, "\n\n<h4>$1</h4>\n\n");
  html = html.replace(/^### (.+)$/gm, "\n\n<h3>$1</h3>\n\n");
  html = html.replace(/^## (.+)$/gm, "\n\n<h2>$1</h2>\n\n");
  html = html.replace(/^# (.+)$/gm, "\n\n<h1>$1</h1>\n\n");

  // Horizontal rules
  html = html.replace(/^---$/gm, "\n\n<hr>\n\n");

  // Bold and italic
  html = html.replace(/\*\*\*(.+?)\*\*\*/g, "<strong><em>$1</em></strong>");
  html = html.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");
  html = html.replace(/\*(.+?)\*/g, "<em>$1</em>");

  // Images (before links)
  html = html.replace(
    /!\[([^\]]*)\]\(([^)]+)\)/g,
    '<img src="$2" alt="$1" loading="lazy">'
  );

  // Links
  html = html.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    '<a href="$2">$1</a>'
  );

  // Blockquotes
  html = html.replace(/^&gt; (.+)$/gm, "\n\n<blockquote><p>$1</p></blockquote>\n\n");

  // Unordered lists · an indented line under an item continues that item
  while (/^([-*] .*)\n[ \t]+(\S.*)$/m.test(html)) {
    html = html.replace(/^([-*] .*)\n[ \t]+(\S.*)$/m, "$1 $2");
  }
  html = html.replace(/^(\s*)[-*] (.+)$/gm, "$1<li>$2</li>");
  html = html.replace(/((?:<li>.*<\/li>\n?)+)/g, "\n\n<ul>$1</ul>\n\n");

  // Ordered lists: an indented line under an item continues it, as in unordered ones
  while (/^(\d+\. .*)\n[ \t]+(\S.*)$/m.test(html)) {
    html = html.replace(/^(\d+\. .*)\n[ \t]+(\S.*)$/m, "$1 $2");
  }
  html = html.replace(/((?:^\d+\. .+\n?)+)/gm, (list) =>
    `\n\n<ol>${list.trim().split("\n").map((l) => `<li>${l.replace(/^\d+\. /, "")}</li>`).join("")}</ol>\n\n`
  );

  // Tables: a blank line ends one, so two tables in a row stay two
  html = html.replace(
    /^\|(.+)\|[ \t]*\n\|[-| :]+\|[ \t]*\n((?:\|.+\|[ \t]*\n?)*)/gm,
    (_, header, body) => {
      const headers = header
        .split("|")
        .map((h) => h.trim())
        .filter(Boolean);
      const rows = body
        .trim()
        .split("\n")
        .map((row) =>
          row
            .split("|")
            .map((c) => c.trim())
            .filter(Boolean)
        );

      let table = "<table><thead><tr>";
      headers.forEach((h) => (table += `<th>${h}</th>`));
      table += "</tr></thead><tbody>";
      rows.forEach((row) => {
        table += "<tr>";
        row.forEach((cell) => (table += `<td>${cell}</td>`));
        table += "</tr>";
      });
      table += "</tbody></table>";
      return `\n\n${table}\n\n`;
    }
  );

  // Paragraphs
  html = html
    .split("\n\n")
    .map((block) => {
      const trimmed = block.trim();
      if (!trimmed) return "";
      if (/^</.test(trimmed)) return trimmed;
      if (/^\x00CODEBLOCK/.test(trimmed)) return trimmed;
      return `<p>${trimmed.replace(/\n/g, " ")}</p>`;
    })
    .join("\n");

  // Restore code blocks and inline code
  codeBlocks.forEach((block, i) => {
    html = html.replace(`\x00CODEBLOCK${i}\x00`, block);
  });
  inlineCodes.forEach((code, i) => {
    html = html.replace(`\x00INLINECODE${i}\x00`, code);
  });

  return html;
}

// ========== Strip markdown for search indexing ==========
function stripMarkdown(md) {
  return md
    .replace(/```[\s\S]*?```/g, "")
    .replace(/`[^`]+`/g, "")
    .replace(/[#*_\[\]()>|`-]/g, "")
    .replace(/\n+/g, " ")
    .trim();
}

// ========== Sidebar: the agents column ==========
// Each section is a space: a fold mark in the faint ink, the name in the space's colour and
// its page count; its pages sit under it the way agents do, in the quieter step of that
// colour, and the page being read is lit in the full step. The page being read also lists
// its own headings under it, the way an agent's row carries its second line.
const folded = new Set();

function renderSidebar() {
  const nav = document.getElementById("sidebar-nav");
  const currentSlug = getCurrentSlug();
  const pages = config.sidebar.reduce((n, s) => n + s.children.length, 0);
  const headings = [...document.querySelectorAll("#article h2")];

  nav.innerHTML = `
    <div class="col-head"><span>docs</span><span class="col-head-count">${pages} pages</span></div>
    <button class="chip" id="sidebar-search"><b>[</b><i>/</i><span>search ${pages} pages</span><b>]</b></button>
    ${config.sidebar
      .map((section, si) => {
        const open = !folded.has(section.title);
        return `
        <div class="space space-${si % SPACE_COUNT}${open ? "" : " folded"}">
          <button class="space-rule" data-section="${esc(section.title)}" aria-expanded="${open}">
            <span class="fold">${open ? "&#9662;" : "&#9656;"}</span><span class="space-name">${section.title}</span><span class="space-count">(${section.children.length})</span>
          </button>
          <div class="space-pages">
            ${section.children
              .map((child) => {
                const active = child.slug === currentSlug;
                const outline = active && headings.length
                  ? `<div class="page-outline">${headings
                      .map((h, hi) => `<button class="outline-row" data-heading="${hi}"><span class="branch">${hi === headings.length - 1 ? "&#9492;" : "&#9500;"}</span>${esc(h.textContent)}</button>`)
                      .join("")}</div>`
                  : "";
                return `<a href="#${child.slug}" class="page-row${active ? " active" : ""}">${mark(active ? "done" : "idle", 11)}<span>${child.title}</span></a>${outline}`;
              })
              .join("")}
          </div>
        </div>`;
      })
      .join("")}
  `;

  nav.querySelectorAll(".space-rule").forEach((btn) => {
    btn.addEventListener("click", () => {
      const t = btn.dataset.section;
      folded.has(t) ? folded.delete(t) : folded.add(t);
      renderSidebar();
      // The sidebar is drawn again; the keyboard stays on the group it just folded.
      [...nav.querySelectorAll(".space-rule")].find((b) => b.dataset.section === t)?.focus();
    });
  });

  nav.querySelectorAll(".outline-row").forEach((btn) => {
    btn.addEventListener("click", () => {
      headings[btn.dataset.heading]?.scrollIntoView({ behavior: "smooth" });
      if (window.innerWidth <= 768) closeSidebar();
    });
  });

  document.getElementById("sidebar-search").addEventListener("click", () => searchEngine.open());

  nav.querySelectorAll(".page-row").forEach((link) => {
    link.addEventListener("click", () => {
      if (window.innerWidth <= 768) {
        closeSidebar();
      }
    });
  });
}

// ========== Home: the top of the introduction ==========
function renderHome(slug) {
  const home = document.getElementById("home");
  const h = config.home;
  if (slug !== config.defaultPage || !h) {
    home.innerHTML = "";
    return;
  }
  const phone = h.phone
    .map((p) => {
      const soon = p.url ? "" : "<em>soon</em>";
      const inner = `${icon(p.icon)}<span class="phone-text"><b>${p.label}</b><small>${p.detail}${soon}</small></span>`;
      const opens = p.download ? "" : ' target="_blank" rel="noopener"';
      return p.url
        ? `<a class="phone-link" href="${p.url}"${opens}>${inner}${icon("chev")}</a>`
        : `<span class="phone-link soon" aria-disabled="true">${inner}</span>`;
    })
    .join("");
  const cards = config.sidebar
    .map(
      (section, si) => `
      <div class="card space-${si % SPACE_COUNT}">
        <div class="card-top">${mark(SECTION_MARKS[si % SECTION_MARKS.length], 36)}<span>${section.children.length} pages</span></div>
        <h2>${section.title}</h2>
        <nav>${section.children.map((c) => `<a href="#${c.slug}"><span>${c.title}</span>${icon("chev")}</a>`).join("")}</nav>
      </div>`
    )
    .join("");

  home.innerHTML = `
    <section class="hero">
      <div class="hero-text">
        <p class="hero-kicker">${config.description}</p>
        <h1>${h.tagline}</h1>
        <p class="hero-lede">${h.lede}</p>
        <div class="install">
          <span class="install-prompt">$</span>
          <code>${esc(h.install)}</code>
          ${copyButton(h.install)}
        </div>
        <p class="phone-head">On your phone</p>
        <div class="phone-links">${phone}</div>
      </div>
      <div class="hero-art">
        <img class="hero-img hero-dark" src="${h.hero.dark}" alt="${esc(h.hero.alt)}">
        <img class="hero-img hero-light" src="${h.hero.light}" alt="${esc(h.hero.alt)}">
      </div>
    </section>
    <section class="cards">${cards}</section>
  `;
}

// ========== Keys panel ==========
// Every row of the tables in keys.md, grouped by their heading, with a live filter. Beside
// every reading page on a wide screen; behind the Keys button on a narrow one.
function parseKeyTables(md) {
  const groups = [];
  let title = "";
  let header = true;
  for (const line of md.split("\n")) {
    if (line.startsWith("## ")) title = line.slice(3).trim();
    if (!line.startsWith("|")) {
      header = true;
      continue;
    }
    if (/^\|[-| :]+\|\s*$/.test(line)) continue;
    const cells = line.trim().slice(1, -1).split("|").map((c) => c.trim());
    if (header) {
      header = false;
      groups.push({ title, rows: [] });
      continue;
    }
    groups[groups.length - 1].rows.push(cells);
  }
  return groups;
}

// One key cell as chips: `ctrl-a n`, `ctrl-a p` is two chords, `ctrl-a 1` .. `ctrl-a 9` a range.
function chords(cell) {
  const codes = [...cell.matchAll(/`([^`]+)`/g)].map((m) => m[1]);
  if (!codes.length) return `<span class="key-word">${esc(cell)}</span>`;
  const sep = cell.includes("..") ? '<span class="key-sep">to</span>' : "";
  return codes
    .map((c) =>
      c.startsWith("ctrl-a ")
        ? `<span class="chord"><kbd class="pre">ctrl-a</kbd><kbd>${esc(c.slice(7))}</kbd></span>`
        : `<span class="chord"><kbd>${esc(c)}</kbd></span>`
    )
    .join(sep);
}

async function renderKeys() {
  const list = document.getElementById("keys-list");
  try {
    const response = await fetch("docs/keys.md");
    if (!response.ok) throw new Error("Not found");
    const groups = parseKeyTables(await response.text());
    list.innerHTML = groups
      .map(
        (g) => `
        <div class="key-group"><h5>${esc(g.title)}</h5>
          ${g.rows
            .map(
              ([k, d]) => `<div class="key-row" data-q="${esc(`${g.title} ${k} ${d}`.toLowerCase())}">
                <span class="key-keys">${chords(k)}</span>
                <span class="key-does">${inlineMarkdown(d)}</span>
              </div>`
            )
            .join("")}
        </div>`
      )
      .join("");
  } catch {
    list.innerHTML = '<p class="keys-empty">The keys page could not be loaded.</p>';
  }
}

function initKeys() {
  const filter = document.getElementById("keys-filter");
  filter.addEventListener("input", () => {
    const q = filter.value.toLowerCase().trim();
    document.querySelectorAll(".key-row").forEach((r) => (r.hidden = !r.dataset.q.includes(q)));
    document.querySelectorAll(".key-group").forEach((g) => (g.hidden = !g.querySelector(".key-row:not([hidden])")));
  });
  // A row shows two lines of what it does; a click reads the rest.
  document.getElementById("keys-list").addEventListener("click", (e) => {
    if (e.target.closest("a")) return;
    e.target.closest(".key-row")?.classList.toggle("open");
  });
  document.getElementById("keys-fab").addEventListener("click", () => document.body.classList.toggle("keys-open"));
  document.getElementById("keys-close").addEventListener("click", () => document.body.classList.remove("keys-open"));
  renderKeys();
}

// ========== Skeleton Loading ==========
function showContentSkeleton() {
  if (!features.skeletonLoading) return;
  const article = document.getElementById("article");
  article.innerHTML = `
    <div class="skeleton" style="width:45%;height:32px;margin-bottom:20px"></div>
    <div class="skeleton" style="width:100%;height:14px;margin-bottom:10px"></div>
    <div class="skeleton" style="width:92%;height:14px;margin-bottom:10px"></div>
    <div class="skeleton" style="width:78%;height:14px;margin-bottom:28px"></div>
    <div class="skeleton" style="width:55%;height:24px;margin-bottom:16px"></div>
    <div class="skeleton" style="width:100%;height:14px;margin-bottom:10px"></div>
    <div class="skeleton" style="width:85%;height:14px;margin-bottom:10px"></div>
  `;
}

// ========== Code Copy Buttons ==========
function addCodeCopyButtons(container) {
  if (!features.codeCopyButtons) return;
  container.querySelectorAll("pre").forEach((pre) => {
    const code = pre.querySelector("code");
    pre.insertAdjacentHTML("beforeend", copyButton(code ? code.textContent : pre.textContent));
  });
}

// ========== Reading Progress ==========
function initReadingProgress() {
  if (!features.readingProgress) return;
  const bar = document.getElementById("reading-progress");
  if (!bar) return;

  const update = () => {
    const docHeight = document.documentElement.scrollHeight - window.innerHeight;
    const progress = docHeight > 0 ? Math.min(window.scrollY / docHeight, 1) : 0;
    bar.style.width = `${progress * 100}%`;
    bar.style.opacity = progress > 0.01 ? "1" : "0";
  };

  window.addEventListener("scroll", update, { passive: true });
  update();
}

// ========== Routing ==========
function getCurrentSlug() {
  return window.location.hash.slice(1) || config.defaultPage;
}

function getPageTitle(slug) {
  for (const section of config.sidebar) {
    const found = section.children.find((c) => c.slug === slug);
    if (found) return found.title;
  }
  return slug;
}

async function loadPage(slug) {
  const article = document.getElementById("article");

  document.body.dataset.page = slug;
  document.body.classList.remove("keys-open");
  renderHome(slug);
  showContentSkeleton();

  try {
    const response = await fetch(`docs/${slug}.md`);
    if (!response.ok) throw new Error("Not found");
    const md = await response.text();
    article.innerHTML = parseMarkdown(md);
    addCodeCopyButtons(article);
  } catch {
    article.innerHTML = `
      <h1>Page Not Found</h1>
      <p>The page <code>${esc(slug)}</code> could not be found.</p>
      <p><a href="#${config.defaultPage}">Go to ${getPageTitle(config.defaultPage)}</a></p>
    `;
  }

  document.title = `${getPageTitle(slug)} · ${config.name}`;
  renderSidebar();
  window.scrollTo(0, 0);
  document.dispatchEvent(new CustomEvent("docs:page"));
}

// ========== Mobile Sidebar ==========
const hamburger = document.getElementById("hamburger");
const sidebar = document.getElementById("sidebar");
const overlay = document.getElementById("overlay");

function openSidebar() {
  sidebar.classList.add("open");
  overlay.classList.add("active");
  hamburger.setAttribute("aria-expanded", "true");
}

function closeSidebar() {
  sidebar.classList.remove("open");
  overlay.classList.remove("active");
  hamburger.setAttribute("aria-expanded", "false");
}

hamburger.addEventListener("click", () => {
  sidebar.classList.contains("open") ? closeSidebar() : openSidebar();
});

overlay.addEventListener("click", closeSidebar);

// ========== Search Indexing ==========
async function buildSearchIndex() {
  const pages = [];

  for (const section of config.sidebar) {
    for (const child of section.children) {
      try {
        const response = await fetch(`docs/${child.slug}.md`);
        if (!response.ok) continue;
        const md = await response.text();
        pages.push({
          title: child.title,
          slug: child.slug,
          content: stripMarkdown(md),
        });
      } catch {
        // Skip pages that can't be fetched
      }
    }
  }

  searchEngine.buildIndex(pages);
}

// ========== Init ==========
function init() {
  renderNavbar();
  initKeys();
  loadPage(getCurrentSlug());
  initReadingProgress();

  window.addEventListener("hashchange", () => {
    loadPage(getCurrentSlug());
  });

  buildSearchIndex();
}

init();
