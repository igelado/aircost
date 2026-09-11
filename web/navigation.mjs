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

  function handleToggle() {
    setOpen(!open);
  }

  function handleDocumentClick(event) {
    if (open && !root.contains(event.target)) {
      close();
    }
  }

  function handleDocumentKeydown(event) {
    if (!open || event.key !== "Escape") {
      return;
    }
    event.preventDefault();
    close({ returnFocus: true });
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
    close();
  }

  toggle.addEventListener("click", handleToggle);
  for (const link of links) {
    link.addEventListener("click", handleLinkClick);
  }
  document.addEventListener("click", handleDocumentClick);
  document.addEventListener("keydown", handleDocumentKeydown);
  if (typeof mobile.addEventListener === "function") {
    mobile.addEventListener("change", handleViewportChange);
  } else {
    mobile.addListener?.(handleViewportChange);
  }
  setOpen(false);

  return Object.freeze({
    close,
    isOpen: () => open,
  });
}
