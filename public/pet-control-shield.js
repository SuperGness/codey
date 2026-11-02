(() => {
  window.__codeyPetControlShieldCleanup?.();

  const enabled = ["__CODEY_SLIM_PET__"][0] === "true";
  // Codex renamed the pet entry points to mini; keys below cover the current
  // mini naming, the older pets naming, and the ChatGPT-side pet manager, so
  // any supported client build keeps the shield active.
  const petControlIds = new Set([
    "settings.mini.show",
    "settings.mini.hide",
    "settings.nav.miniAndPets",
    "settings.personalization.mini.wake",
    "settings.personalization.mini.tuckAway",
    "codex.profileFooter.showMini",
    "codex.profileFooter.hideMini",
    "codex.command.showMiniOverlay",
    "codex.command.hideMiniOverlay",
    "codex.command.wakeMiniOverlay",
    "codex.commandDescription.showMiniOverlay",
    "codex.commandDescription.tuckAwayMiniOverlay",
    "codex.localConversation.remoteHostedPip.sendToPet",
    "settings.appearance.pets",
    "settings.personalization.pets",
    "settings.pets",
    "settings.nav.pets",
    "settings.section.pets",
    "settings.personalization.pets.openPet",
    "settings.personalization.pets.tuckAwayPet",
    "codex.profileFooter.showPet",
    "codex.profileFooter.hidePet",
    "codex.command.openPetOverlay",
    "codex.command.tuckAwayPetOverlay",
  ]);
  const petControlIdPrefixes = [
    "settings.mini.",
    "settings.nav.mini",
    "settings.personalization.mini.",
    "settings.chatGpt.personalization.pet.",
    "settings.appearance.pets.",
    "settings.nav.pets.",
    "settings.personalization.pets.",
    "settings.section.pets.",
    "settings.pets.",
  ];
  // Exact-match only: matching "mini" as a substring would also hide model
  // options such as "GPT-5.4-Mini" in the model picker.
  const petControlLabels = [
    "pet", "pets", "wake pet", "show pet", "tuck away pet", "hide pet",
    "refresh custom pets", "create your own pet", "open custom pets folder",
    "宠物", "唤醒宠物", "显示宠物", "收起宠物", "隐藏宠物",
    "刷新自定义宠物", "创建自己的宠物", "打开自定义宠物文件夹",
    "寵物", "喚醒寵物", "顯示寵物", "收起寵物", "隱藏寵物",
    "重新整理自訂寵物", "建立自己的寵物", "開啟自訂寵物資料夾",
    "mini", "minis", "wake mini", "show mini", "tuck away mini", "hide mini",
    "refresh custom minis", "create your own mini", "open custom minis folder",
    "迷你", "唤醒 Mini", "显示 Mini", "收起 Mini", "隐藏 Mini",
    "刷新自定义 Mini", "创建自己的 Mini", "打开自定义 Mini 文件夹",
    "喚醒 Mini", "顯示 Mini", "隱藏 Mini",
    "重新整理自訂 Mini", "建立自己的 Mini", "開啟自訂 Mini 資料夾",
  ];
  const fallbackLabelPattern = new RegExp(
    `^(?:${petControlLabels.join("|")})$`,
    "i",
  );
  const controlSelector = "button, [role=button], [role=menuitem], [role=option], [role=tab]";

  const isPetControlId = (value) =>
    petControlIds.has(value)
      || petControlIdPrefixes.some((prefix) => value.startsWith(prefix));

  const isPetControl = (control) => {
    if (!(control instanceof HTMLElement)) return false;
    const descriptor = window.__codeyMutationDispatcher.controlDescriptor(control);
    if (fallbackLabelPattern.test(descriptor)) return true;

    // Deliberately not memoised. React reuses host elements and swaps both
    // __reactProps$ and __reactFiber$ independently, so the walk runs on every
    // live control. The lifecycle's mutation filtering and scan budgets keep
    // streamed turns away from this path.
    return window.__codeySharedRuntime.reactInternalGraphIncludes(
      control,
      isPetControlId,
    );
  };

  const createElementWalker = (root) => {
    let createTreeWalker = null;
    try {
      createTreeWalker = root?.ownerDocument?.createTreeWalker?.bind(root.ownerDocument)
        || (typeof document?.createTreeWalker === "function"
          ? document.createTreeWalker.bind(document)
          : null);
    } catch {
      return null;
    }
    if (!createTreeWalker) return null;
    try {
      const walker = createTreeWalker(root, 1 | 0x100 | 0x200 | 0x400);
      walker.currentNode = root;
      return walker;
    } catch {
      return null;
    }
  };

  // The fallback reads only the current node's child, sibling and parent links.
  // It never materialises the subtree, so a frame can stop between any two
  // nodes without paying a full DOM lookup first.
  const nextElement = (root, node) => {
    const children = node?.children;
    if (children?.[0]) return children[0];
    let current = node;
    while (current && current !== root) {
      const next = current.nextElementSibling;
      if (next) return next;
      current = current.parentElement;
    }
    return null;
  };

  const block = (root = document, options = {}) => {
    const maxNodes = Number.isInteger(options.maxNodes)
      ? Math.max(1, options.maxNodes)
      : Number.isInteger(options.maxControls)
        ? Math.max(1, options.maxControls)
        : Number.MAX_SAFE_INTEGER;
    if (!enabled) return 0;
    const traversalRoot = !(root instanceof HTMLElement)
      && root?.documentElement instanceof HTMLElement
      ? root.documentElement
      : root;
    const initialCursor = options.startCursor || null;
    if (initialCursor && !root?.contains?.(initialCursor)) {
      options.startCursor = null;
    }

    const walker = createElementWalker(traversalRoot);
    let node = options.startCursor || traversalRoot;
    let blocked = 0;
    let controlsInspected = 0;
    let nodesVisited = 0;
    let complete = false;
    let resumed = Boolean(options.startCursor);

    const visit = (current) => {
      nodesVisited += 1;
      if (!(current instanceof HTMLElement) || !current.matches?.(controlSelector)) {
        return;
      }
      controlsInspected += 1;
      if (!isPetControl(current)) return;
      const fullyBlocked = current.getAttribute("data-codey-pet-control-blocked") === "true"
        && current.getAttribute("aria-hidden") === "true"
        && current.getAttribute("tabindex") === "-1"
        && current.getAttribute("inert") !== null
        && String(current.style.display || "").startsWith("none")
        && (!("disabled" in current) || current.disabled);
      if (!fullyBlocked) {
        current.setAttribute("data-codey-pet-control-blocked", "true");
        current.setAttribute("aria-hidden", "true");
        current.setAttribute("tabindex", "-1");
        current.setAttribute("inert", "");
        current.style.setProperty("display", "none", "important");
        if ("disabled" in current && !current.disabled) current.disabled = true;
      }
      blocked += 1;
    };

    if (!resumed && node !== null) visit(node);
    while (nodesVisited < maxNodes) {
      if (resumed) {
        // A saved cursor means the previous frame already visited it. Seek
        // to it and advance once without charging the cursor to this frame.
        resumed = false;
        if (walker) {
          try {
            walker.currentNode = node;
            node = walker.nextNode();
          } catch {
            node = nextElement(traversalRoot, node);
          }
        } else {
          node = nextElement(traversalRoot, node);
        }
      } else if (walker) {
        try {
          node = walker.nextNode();
        } catch {
          options.startCursor = null;
          walker.currentNode = traversalRoot;
          block(root, options);
          return blocked;
        }
      } else {
        node = nextElement(traversalRoot, node);
      }
      if (!node) {
        complete = true;
        break;
      }
      visit(node);
    }

    options.onProgress?.({
      complete,
      controlsInspected,
      nextCursor: complete ? null : node,
      nodesVisited,
    });
    return blocked;
  };

  if (!enabled) {
    window.__codeyBlockNativePetControls = () => 0;
    window.__codeyPetControlShield = Object.freeze({
      enabled,
      block: () => 0,
      isPetControl,
    });
    window.__codeyPetControlShieldCleanup = () => {
      delete window.__codeyBlockNativePetControls;
      delete window.__codeyPetControlShield;
      delete window.__codeyPetControlShieldCleanup;
    };
    return;
  }

  const shieldLifecycle = window.__codeyMutationDispatcher?.createShieldLifecycle({
    attributeFilter: ["aria-label", "role", "title"],
    block,
    eventSelector: controlSelector,
    isControl: isPetControl,
    probeNodeLimit: 64,
  });
  window.__codeyBlockNativePetControls = block;
  window.__codeyPetControlShield = Object.freeze({ enabled, block, isPetControl });
  window.__codeyPetControlShieldCleanup = () => {
    shieldLifecycle?.cleanup();
    delete window.__codeyBlockNativePetControls;
    delete window.__codeyPetControlShield;
    delete window.__codeyPetControlShieldCleanup;
  };
  block();
})();
