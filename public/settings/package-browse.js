// ABOUTME: Provides the Settings > Extensions Community package browser.
// ABOUTME: Keeps registry state, filtering, pagination, and install actions behind explicit dependencies.

export function setupPackageBrowse({
  root = document,
  transport,
  nativeAvailable,
  t,
  createIcon,
  renderPackageInstallFailure,
  setExtensionActionButton,
  onInstalledChanged,
  catalogUrl = "https://raw.githubusercontent.com/palandatarxcom/Picot/main/community-extensions.json",
}) {
  // Catalog is a committed snapshot refreshed manually via
  // scripts/build_extension_catalog.py; see that script before changing the
  // expected schema or fields.
  const PKG_CATALOG_URL = catalogUrl;
  const browseListEl = root.getElementById("pkg-browse-list");
  const browseSearchEl = root.getElementById("pkg-browse-search");
  const browsePillsEl = root.getElementById("pkg-browse-pills");
  const browseCountEl = root.getElementById("pkg-browse-count");
  let browsePaginationEl = root.getElementById("pkg-browse-pagination");
  if (!browsePaginationEl && browseListEl && browseListEl.parentNode) {
    browsePaginationEl = document.createElement("div");
    browsePaginationEl.className = "pkg-browse-pagination";
    browsePaginationEl.id = "pkg-browse-pagination";
    browsePaginationEl.hidden = true;
    browseListEl.parentNode.insertBefore(browsePaginationEl, browseListEl.nextSibling);
  }
  const browseSortEl = root.getElementById("pkg-browse-sort");

  let browseAllPackages = null;
  let browseInstalledSet = new Set();
  let browseLoaded = false;
  let browseLoading = false;
  let browseActiveType = "all";
  let browseSearchQuery = "";
  let browseSortMode = "downloads";
  let browseSearchTimer = null;
  let browsePage = 1;
  const BROWSE_PAGE_SIZE = 50;

  async function loadBrowsePackages(force = false) {
    if (!browseListEl) return;
    if (browseLoading) return;
    if (browseLoaded && !force) {
      renderBrowsePackages();
      return;
    }
    browseLoading = true;
    const loading = document.createElement("div");
    loading.className = "settings-api-keys-loading pkg-browse-full-row";
    loading.textContent = t("extensions.loadingPackages");
    browseListEl.replaceChildren(loading);
    try {
      const [packages, installed] = await Promise.all([
        fetchBrowsePackages(),
        fetchInstalledSources(),
      ]);
      browseAllPackages = packages;
      browseInstalledSet = installed;
      browseLoaded = true;
      renderBrowsePackages();
    } catch (err) {
      const message = String(err?.message || err || t("extensions.failedToLoadPackages"));
      const error = document.createElement("div");
      error.className = "settings-api-keys-empty pkg-browse-full-row";
      const messageText = document.createElement("span");
      messageText.textContent = message;
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "settings-value-btn";
      retry.id = "pkg-browse-retry";
      retry.textContent = t("actions.retry");
      retry.addEventListener("click", () => loadBrowsePackages(true));
      error.append(messageText, document.createTextNode(" "), retry);
      browseListEl.replaceChildren(error);
    } finally {
      browseLoading = false;
    }
  }

  async function fetchBrowsePackages() {
    const res = await fetch(PKG_CATALOG_URL, { headers: { Accept: "application/json" } });
    if (!res.ok) throw new Error(`Catalog fetch returned ${res.status}`);
    const data = await res.json();
    if (!Array.isArray(data?.packages)) throw new Error("Catalog response has no packages list");
    return data.packages;
  }

  async function fetchInstalledSources() {
    if (!nativeAvailable()) return new Set();
    try {
      const configured = await transport.listPiPackages();
      return new Set(
        Array.isArray(configured)
          ? configured
              .map((entry) => (typeof entry === "string" ? entry : entry?.source))
              .filter(Boolean)
          : [],
      );
    } catch {
      return new Set();
    }
  }

  function browseSourceFor(pkg) {
    return `npm:${pkg.name}`;
  }

  function normalizeRepoUrl(url) {
    if (!url) return null;
    return url
      .replace(/^git\+/, "")
      .replace(/^git:\/\//, "https://")
      .replace(/^git@github\.com:/, "https://github.com/")
      .replace(/\.git$/, "");
  }

  function openExternalLink(url) {
    if (!url) return;
    if (nativeAvailable()) {
      transport.openExternal(url).catch((err) => {
        console.error("[browse] failed to open external link:", err);
      });
      return;
    }
    // Non-native (LAN/mobile): no native opener and no popup window. Show a
    // transient inline toast with a clickable link the user can follow.
    showExternalLinkToast(url);
  }

  function showExternalLinkToast(url) {
    const host = root.body;
    if (!host) return;
    const toast = document.createElement("div");
    toast.className = "external-link-toast";
    const label = document.createElement("span");
    label.textContent = t("browse.openExternalPrompt");
    const link = document.createElement("a");
    link.className = "external-link-toast-link";
    link.href = url;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.textContent = url;
    toast.append(label, link);
    host.appendChild(toast);
    setTimeout(() => toast.remove(), 8000);
  }

  function createBrowseIcon(kind) {
    const iconName = kind === "link" ? "link" : "external-link";
    return createIcon(iconName, { size: 14 });
  }

  function createBrowseLinkButton(kind, label, url) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "pkg-browse-link";
    btn.title = label;
    btn.setAttribute("aria-label", label);
    const labelElement = document.createElement("span");
    labelElement.textContent = label;
    btn.append(createBrowseIcon(kind), labelElement);
    btn.addEventListener("click", (event) => {
      event.stopPropagation();
      openExternalLink(url);
    });
    return btn;
  }

  function buildBrowseLinks(pkg) {
    const links = pkg.links || {};
    const container = document.createElement("div");
    container.className = "pkg-browse-links";

    const npmUrl = links.npm || `https://www.npmjs.com/package/${encodeURIComponent(pkg.name)}`;
    container.appendChild(createBrowseLinkButton("link", "npm", npmUrl));

    const repo = normalizeRepoUrl(links.repository);
    if (repo) {
      const isGithub = /github\.com/i.test(repo);
      container.appendChild(createBrowseLinkButton("link", isGithub ? "GitHub" : "repo", repo));
    }

    const homepage = normalizeRepoUrl(links.homepage);
    if (homepage && homepage !== repo) {
      container.appendChild(createBrowseLinkButton("link", "homepage", homepage));
    }

    return container;
  }

  function browseUpdatedTime(pkg) {
    const raw = pkg.updatedAt || pkg.updated || pkg.modified || pkg.date || pkg.time || 0;
    const t = typeof raw === "number" ? raw : Date.parse(raw);
    return Number.isFinite(t) ? t : 0;
  }

  function sortBrowsePackages(packages) {
    const sorted = packages.slice();
    switch (browseSortMode) {
      case "name":
        sorted.sort((a, b) =>
          (a.name || "").localeCompare(b.name || "", undefined, {
            sensitivity: "base",
          }),
        );
        break;
      case "updated":
        sorted.sort((a, b) => browseUpdatedTime(b) - browseUpdatedTime(a));
        break;
      default:
        sorted.sort((a, b) => (b.downloads || 0) - (a.downloads || 0));
        break;
    }
    return sorted;
  }

  function filterBrowsePackages() {
    if (!browseAllPackages) return [];
    const query = browseSearchQuery.toLowerCase().trim();
    const filtered = browseAllPackages.filter((pkg) => {
      if (browseActiveType !== "all") {
        if (!Array.isArray(pkg.types) || !pkg.types.includes(browseActiveType)) return false;
      }
      if (query) {
        const inName = pkg.name.toLowerCase().includes(query);
        const inDesc = (pkg.description || "").toLowerCase().includes(query);
        const inAuthor = (pkg.author || "").toLowerCase().includes(query);
        if (!inName && !inDesc && !inAuthor) return false;
      }
      return true;
    });
    return sortBrowsePackages(filtered);
  }

  function renderBrowsePackages() {
    if (!browseListEl) return;
    const results = filterBrowsePackages();

    const totalPages = Math.max(1, Math.ceil(results.length / BROWSE_PAGE_SIZE));
    if (browsePage > totalPages) browsePage = totalPages;
    if (browsePage < 1) browsePage = 1;
    const start = (browsePage - 1) * BROWSE_PAGE_SIZE;
    const pageResults = results.slice(start, start + BROWSE_PAGE_SIZE);

    if (browseCountEl) {
      if (results.length === 0) {
        browseCountEl.textContent = t("extensions.browseCountZero", { total: results.length });
      } else {
        const rangeStart = start + 1;
        const rangeEnd = start + pageResults.length;
        browseCountEl.textContent = t("extensions.browseCountRange", {
          start: rangeStart,
          end: rangeEnd,
          total: results.length,
        });
      }
    }

    browseListEl.replaceChildren();
    if (!results.length) {
      const empty = document.createElement("div");
      empty.className = "settings-api-keys-empty pkg-browse-full-row";
      empty.textContent = t("extensions.noPackagesMatch");
      browseListEl.appendChild(empty);
      renderBrowsePagination(totalPages);
      return;
    }
    for (const pkg of pageResults) {
      browseListEl.appendChild(createBrowseRow(pkg));
    }
    renderBrowsePagination(totalPages);
  }

  function renderBrowsePagination(totalPages) {
    if (!browsePaginationEl) return;
    if (totalPages <= 1) {
      browsePaginationEl.hidden = true;
      browsePaginationEl.replaceChildren();
      return;
    }
    browsePaginationEl.hidden = false;
    browsePaginationEl.replaceChildren();

    const goTo = (page) => {
      browsePage = page;
      renderBrowsePackages();
      if (browseListEl) browseListEl.scrollIntoView({ block: "nearest" });
    };

    const addBtn = (label, page, { active = false, disabled = false } = {}) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = `pkg-browse-page-btn${active ? " is-active" : ""}`;
      btn.textContent = label;
      btn.disabled = disabled;
      if (!disabled && !active) btn.addEventListener("click", () => goTo(page));
      browsePaginationEl.appendChild(btn);
      return btn;
    };

    const addEllipsis = () => {
      const span = document.createElement("span");
      span.className = "pkg-browse-page-ellipsis";
      span.textContent = "…";
      browsePaginationEl.appendChild(span);
    };

    const previous = addBtn("", browsePage - 1, { disabled: browsePage <= 1 });
    const previousIcon = createIcon("chevron-left", { size: 14 });
    if (previousIcon) previous.replaceChildren(previousIcon);

    const pages = new Set([1, totalPages, browsePage]);
    for (let d = 1; d <= 2; d++) {
      pages.add(browsePage - d);
      pages.add(browsePage + d);
    }
    const visible = [...pages].filter((p) => p >= 1 && p <= totalPages).sort((a, b) => a - b);

    let prev = 0;
    for (const p of visible) {
      if (p - prev > 1) addEllipsis();
      addBtn(String(p), p, { active: p === browsePage });
      prev = p;
    }

    const next = addBtn("", browsePage + 1, { disabled: browsePage >= totalPages });
    const nextIcon = createIcon("chevron-right", { size: 14 });
    if (nextIcon) next.replaceChildren(nextIcon);
  }

  function createBrowseRow(pkg) {
    const source = browseSourceFor(pkg);
    const installed = browseInstalledSet.has(source);

    const row = document.createElement("div");
    row.className = "settings-extension-row pkg-browse-row";

    const info = document.createElement("div");
    info.className = "settings-extension-info";

    const name = document.createElement("div");
    name.className = "settings-extension-name";
    name.textContent = pkg.name;
    info.appendChild(name);

    if (pkg.description) {
      const description = document.createElement("div");
      description.className = "settings-extension-description";
      description.textContent = pkg.description;
      info.appendChild(description);
    }

    const badges = document.createElement("div");
    badges.className = "pkg-browse-badges";
    for (const t of pkg.types || []) {
      const badge = document.createElement("span");
      badge.className = "pkg-browse-badge";
      badge.dataset.type = t;
      badge.textContent = t;
      badges.appendChild(badge);
    }
    const downloads = document.createElement("span");
    downloads.className = "pkg-browse-meta";
    downloads.textContent = t("extensions.downloadsPerMonth", {
      count: (pkg.downloads || 0).toLocaleString(),
    });
    badges.appendChild(downloads);
    info.appendChild(badges);

    const status = document.createElement("div");
    status.className = "settings-extension-status";
    status.hidden = true;
    info.appendChild(status);

    info.appendChild(buildBrowseLinks(pkg));

    const actions = document.createElement("div");
    actions.className = "settings-extension-actions";
    const button = document.createElement("button");
    button.type = "button";
    // Install is the affirmative action (solid accent); uninstall reuses the
    // danger ghost the Installed tab's Remove button already uses, so the two
    // states differ by fill weight as well as hue.
    button.className = `settings-value-btn${installed ? " is-danger" : " is-primary"}`;

    const canManage = nativeAvailable();
    if (!canManage) {
      button.disabled = true;
      setExtensionActionButton(button, t("extensions.desktopOnly"));
    } else {
      setExtensionActionButton(button, installed ? t("actions.uninstall") : t("actions.install"));
      button.addEventListener("click", async () => {
        button.disabled = true;
        button.classList.add("loading");
        const previous = installed ? t("actions.uninstall") : t("actions.install");
        setExtensionActionButton(
          button,
          installed ? t("status.uninstalling") : t("status.installing"),
          true,
        );
        status.hidden = false;
        status.classList.remove("is-error");
        status.textContent = installed ? t("status.removing") : t("status.installing");
        status.title = status.textContent;
        try {
          if (installed) {
            await transport.removePiPackage(source);
            browseInstalledSet.delete(source);
            onInstalledChanged?.(source);
          } else {
            await transport.installPiPackage(source);
            browseInstalledSet.add(source);
            onInstalledChanged?.(source);
          }
          renderBrowsePackages();
        } catch (err) {
          renderPackageInstallFailure(status, err, installed ? "uninstall" : "install");
          button.disabled = false;
          button.classList.remove("loading");
          setExtensionActionButton(button, previous);
        }
      });
    }
    actions.appendChild(button);

    row.appendChild(info);
    row.appendChild(actions);
    return row;
  }

  if (browsePillsEl) {
    browsePillsEl.addEventListener("click", (event) => {
      const pill = event.target.closest(".pkg-browse-pill");
      if (!pill) return;
      browseActiveType = pill.dataset.pkgType || "all";
      for (const p of browsePillsEl.querySelectorAll(".pkg-browse-pill")) {
        p.classList.toggle("active", p === pill);
      }
      browsePage = 1;
      renderBrowsePackages();
    });
  }

  if (browseSearchEl) {
    browseSearchEl.addEventListener("input", () => {
      clearTimeout(browseSearchTimer);
      browseSearchTimer = setTimeout(() => {
        browseSearchQuery = browseSearchEl.value;
        browsePage = 1;
        renderBrowsePackages();
      }, 180);
    });
  }

  if (browseSortEl) {
    browseSortEl.value = browseSortMode;
    browseSortEl.addEventListener("change", () => {
      browseSortMode = browseSortEl.value || "downloads";
      browsePage = 1;
      renderBrowsePackages();
    });
  }

  return {
    load: loadBrowsePackages,
    refresh: () => loadBrowsePackages(true),
    render: renderBrowsePackages,
    getInstalledSources: () => new Set(browseInstalledSet),
  };
}
