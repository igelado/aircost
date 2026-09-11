export const MOBILE_NAVIGATION_QUERY = "(max-width: 760px)";

export function isPlainPrimaryClick(event) {
  return !event.defaultPrevented
    && event.button === 0
    && !event.metaKey
    && !event.ctrlKey
    && !event.shiftKey
    && !event.altKey;
}

export function createTaskNavigation({
  root,
  toggle,
  menu,
  links,
  document,
  matchMedia,
  navigate,
}) {
  if (
    !root
    || !toggle
    || !menu
    || !Array.isArray(links)
    || !document
    || typeof matchMedia !== "function"
    || typeof navigate !== "function"
  ) {
    throw new Error("Task navigation requires its root, toggle, menu, links, document, media query, and router callback.");
  }

  const mobile = matchMedia(MOBILE_NAVIGATION_QUERY);
  let navigationFocusOwner = "outside";
  let open = false;

  function setOpen(next) {
    open = Boolean(next) && mobile.matches;
    root.classList.toggle("is-menu-open", open);
    toggle.setAttribute("aria-expanded", String(open));
  }

  function close({ returnFocus = false } = {}) {
    setOpen(false);
    if (returnFocus && mobile.matches) {
      toggle.focus();
    }
  }

  function closeForRouteActivation() {
    const returnFocus = open && menu.contains(document.activeElement);
    close({ returnFocus });
  }

  function handleToggle() {
    setOpen(!open);
  }

  function handleDocumentClick(event) {
    if (open && !root.contains(event.target)) {
      close({ returnFocus: menu.contains(document.activeElement) });
    }
  }

  function handleDocumentKeydown(event) {
    if (!open || event.key !== "Escape") {
      return;
    }
    event.preventDefault();
    close({ returnFocus: true });
  }

  function handleDocumentFocus(event) {
    if (event.target === toggle) {
      navigationFocusOwner = "toggle";
    } else if (menu.contains(event.target)) {
      navigationFocusOwner = "menu";
    } else {
      navigationFocusOwner = "outside";
    }
  }

  function handleLinkClick(event) {
    if (!isPlainPrimaryClick(event)) {
      return;
    }
    event.preventDefault();
    if (navigate(event.currentTarget) !== false && mobile.matches) {
      close({ returnFocus: true });
    }
  }

  function handleViewportChange() {
    if (
      !mobile.matches
      && (document.activeElement === toggle || navigationFocusOwner === "toggle")
    ) {
      close();
      const destination = links.find((link) => link.getAttribute?.("aria-current") === "page")
        ?? links[0];
      destination?.focus();
      return;
    }
    close({
      returnFocus: mobile.matches
        && (menu.contains(document.activeElement) || navigationFocusOwner === "menu"),
    });
  }

  toggle.addEventListener("click", handleToggle);
  for (const link of links) {
    link.addEventListener("click", handleLinkClick);
  }
  document.addEventListener("click", handleDocumentClick);
  document.addEventListener("focusin", handleDocumentFocus);
  document.addEventListener("keydown", handleDocumentKeydown);
  if (typeof mobile.addEventListener === "function") {
    mobile.addEventListener("change", handleViewportChange);
  } else {
    mobile.addListener?.(handleViewportChange);
  }
  root.classList.toggle("is-menu-ready", true);
  setOpen(false);

  return Object.freeze({
    close,
    closeForRouteActivation,
    isOpen: () => open,
  });
}
