// ABOUTME: Shared model-selector dropdown menu used by both the main composer
// ABOUTME: and the ephemeral (Side/Quick Chat) composers, so the two surfaces
// ABOUTME: stay visually and behaviorally identical (search, scoped sections,
// ABOUTME: star toggle, empty state).

import { splitModelsByScope } from "./selection.js";

/**
 * Render one dropdown opening into `menu` (emptied first).
 *
 * - `loadModels()` returns the visible model list synchronously after the
 *   caller has warmed it (main composer: host cache; ephemeral views: their
 *   own runtime query filtered by catalog visibility).
 * - `configGateway` is optional; when present, models are split into scoped
 *   (★, persisted via `list_scoped_models` / `set_scoped_model`) and remaining
 *   sections. Without it, a flat list renders.
 * - `onPick(model)` fires on item click; closing the menu is the caller's job.
 * - `onOpenSettingsClick` powers the empty-state button when no model is
 *   available (e.g. no API keys configured).
 * - `close()` is invoked when the user presses Escape in the search box.
 */
export function openModelDropdownMenu({
  doc,
  dropdown,
  menu,
  loadModels,
  isSelected,
  onPick,
  configGateway = null,
  onOpenSettingsClick = null,
  modelsUnavailable = false,
  close = null,
  t,
}) {
  menu.replaceChildren();

  const search = doc.createElement("input");
  search.className = "model-dropdown-search";
  search.placeholder = t("models.searchPlaceholder");
  search.type = "text";
  menu.appendChild(search);

  const itemsContainer = doc.createElement("div");
  itemsContainer.className = "model-dropdown-items";
  menu.appendChild(itemsContainer);

  let scopedModelIds = [];

  const renderEmpty = () => {
    const empty = doc.createElement("div");
    empty.className = "model-dropdown-empty";
    const content = doc.createElement("div");
    content.style.cssText = "padding:14px;color:var(--text-dim);font-size:12px;line-height:1.5";
    const title = doc.createElement("div");
    title.style.cssText = "color:var(--text-primary);margin-bottom:6px";
    title.textContent = modelsUnavailable ? t("models.unavailableTitle") : t("models.emptyTitle");
    const help = doc.createElement("div");
    help.textContent = modelsUnavailable ? t("models.unavailableHelp") : t("models.emptyHelp");
    content.append(title, help);
    if (onOpenSettingsClick) {
      const settingsButton = doc.createElement("button");
      settingsButton.type = "button";
      settingsButton.className = "btn-primary";
      settingsButton.style.marginTop = "10px";
      settingsButton.textContent = t("settings.openSettings");
      settingsButton.addEventListener("click", () => {
        onOpenSettingsClick();
      });
      content.appendChild(settingsButton);
    }
    empty.appendChild(content);
    itemsContainer.appendChild(empty);
  };

  const appendSection = (models, label, isScoped) => {
    if (models.length === 0) return;
    const heading = doc.createElement("div");
    heading.className = "model-dropdown-section";
    heading.textContent = label;
    itemsContainer.appendChild(heading);
    for (const model of models) {
      const el = doc.createElement("div");
      const selected = Boolean(isSelected(model));
      el.className = `model-dropdown-item${selected ? " active" : ""}`;
      const name = doc.createElement("span");
      name.textContent = model.id.replace(/-\d{8}$/, "");
      if (model.provider && model.provider !== "anthropic") {
        const provider = doc.createElement("span");
        provider.className = "model-dropdown-item-provider";
        provider.textContent = model.provider;
        name.appendChild(provider);
      }
      const context = doc.createElement("span");
      context.className = "model-dropdown-item-ctx";
      context.textContent = model.contextWindow
        ? `${(model.contextWindow / 1000).toFixed(0)}k`
        : "";
      el.append(name, context);
      if (configGateway) {
        const star = doc.createElement("button");
        star.type = "button";
        star.className = `model-dropdown-star${isScoped ? " active" : ""}`;
        star.textContent = isScoped ? "★" : "☆";
        star.setAttribute("aria-label", t(isScoped ? "models.removeScoped" : "models.addScoped"));
        star.addEventListener("click", async (event) => {
          event.stopPropagation();
          try {
            const response = await configGateway.call("set_scoped_model", {
              provider: model.provider,
              modelId: model.id,
              enabled: !isScoped,
            });
            if (response?.ok && Array.isArray(response.data?.modelIds)) {
              scopedModelIds = response.data.modelIds;
              renderItems(search.value);
            } else {
              console.warn("[Models] set_scoped_model rejected:", response?.error ?? response);
            }
          } catch (error) {
            // A swallowed rejection here reads as a dead star button; surface it.
            console.warn("[Models] set_scoped_model failed:", error);
          }
        });
        el.appendChild(star);
      }
      el.addEventListener("click", () => {
        onPick(model);
      });
      itemsContainer.appendChild(el);
    }
  };

  const renderItems = (filter) => {
    itemsContainer.replaceChildren();
    const models = loadModels() || [];
    const query = (filter || "").toLowerCase();
    if (models.length === 0) {
      renderEmpty();
      return;
    }
    const matching = models.filter((m) => {
      const shortName = m.id.replace(/-\d{8}$/, "");
      const providerStr = m.provider || "";
      return (
        !query ||
        shortName.toLowerCase().includes(query) ||
        providerStr.toLowerCase().includes(query)
      );
    });
    const { scoped, remaining } = splitModelsByScope(matching, scopedModelIds);
    appendSection(scoped, t("models.scoped"), true);
    appendSection(remaining, t("models.allEnabled"), false);
  };

  const loadScoped = async () => {
    if (!configGateway) {
      renderItems("");
      return;
    }
    try {
      const response = await configGateway.call("list_scoped_models");
      if (response?.ok && Array.isArray(response.data?.modelIds)) {
        scopedModelIds = response.data.modelIds;
      }
    } catch (error) {
      // An unavailable config bridge degrades to the unscoped enabled list.
      console.warn("[Models] list_scoped_models failed:", error);
    }
    renderItems(search.value);
  };

  search.addEventListener("input", () => renderItems(search.value));
  search.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      close?.();
    } else if (event.key === "Enter") {
      itemsContainer.querySelector(".model-dropdown-item")?.click();
    }
  });

  dropdown.classList.add("open");
  menu.classList.remove("hidden");
  void loadScoped();
  requestAnimationFrame(() => search.focus());
}
