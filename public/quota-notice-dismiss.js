// Adds an accessible close control to native Codex, Work and Team quota notices.
(() => {
  if (window.__codeyQuotaNoticeDismissInstalled) return;
  window.__codeyQuotaNoticeDismissInstalled = true;

  const styleId = "codey-quota-notice-dismiss-style";
  const noticeAttribute = "data-codey-quota-notice";
  const dismissedAttribute = "data-codey-quota-notice-dismissed";
  const closeAttribute = "data-codey-quota-notice-close";
  const controlSelector = "button, [role=button], a[href]";
  const conversationTurnSelector = "[data-turn-key]";
  const warningSurfaceSelector = [
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    ".ProseMirror",
    "[contenteditable='true']",
    "[role='textbox']",
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "[data-radix-portal]",
    "[data-portal]",
  ].join(", ");
  const scanningRootLimit = 24;
  const batchElementLimit = 160;
  const scanDelayMs = 40;
  const nativeDismissLabels = [
    /^(?:dismiss(?:\s+usage\s+banner)?|关闭|關閉|不再显示用量横幅|關閉用量橫幅)$/i,
  ];
  const controlLabels = [
    /^add\s+credits$/i,
    /^request\s+(?:increase|more\s+usage)$/i,
    /^increase\s+usage\s+limit$/i,
    /^notify\s+admin$/i,
    /^(?:use\s+available\s+reset|reset\s+usage(?:\s+limit)?)$/i,
    /^(?:添加额度|充值额度|新增點數)$/,
    /^(?:申请提高限额|申请增加用量|申请更多使用额度|申請提高使用上限|申請更多使用額度)$/,
    /^(?:提升使用限额|提高使用額度上限)$/,
    /^(?:通知管理员|通知管理員)$/,
    /^(?:使用可用重置|重置使用量)$/,
  ];
  const actionControlLabels = [
    ...controlLabels,
    /^(?:[^\p{L}\p{N}]*\s*)?(?:upgrade(?:\s+(?:plan|to\s+(?:pro|plus|go)))?|rejoin\s+pro|continue\s+with\s+plus|view\s+plans|升级(?:\s*套餐|至\s*(?:pro|plus|go)(?:\s*继续使用)?)?|重新订阅\s*pro|查看套餐|升級(?:\s*方案|至\s*(?:pro|plus|go)(?:\s*以繼續)?)?|重新訂閱\s*pro|查看方案)$/iu,
  ];
  const titlePatterns = [
    /you(?:'ve|\s+have)\s+(?:hit|reached)\s+your\s+usage\s+limits?/i,
    /you(?:'re|\s+are)\s+approaching\s+your\s+usage\s+limit/i,
    /you(?:'re|\s+are)\s+out\s+of\s+(?:codex\s+(?:and\s+work\s+)?)?usage/i,
    /used\s+(?:at\s+least\s+)?\d+%\s+(?:or\s+more\s+)?of\s+your\s+usage\s+limit/i,
    /(?:codex\s+(?:and|&)\s+work\s+(?:(?:usage|rate)\s+)?|(?:codex\s+)?(?:usage|rate)\s+)limits?\s+(?:reached|exhausted)/i,
    /(?:codex(?:\s*(?:和|与|及|and|&)\s*(?:work|工作(?:区)?))?\s*)?(?:你已达(?:到)?使用(?:额度)?上限|已达到使用额度上限|你已達(?:到)?使用上限|使用额度已用完)/i,
    /你的用量即将达到上限|你即將達到使用上限/,
    /用量已达到或超过限额的\s*\d+%|使用量已達上限的\s*\d+%\s*以上|(?:已使用|已用掉)至少\s*\d+%\s*的(?:用量限额|使用額度)/,
  ];
  const noticeSupportPatterns = [
    /review\s+your\s+workspace(?:'s)?\s+usage\s+settings/i,
    /contact\s+your\s+workspace\s+owner/i,
    /request\s+an\s+increase\s+from\s+your\s+workspace\s+admin/i,
    /increase\s+your\s+usage\s+limit\s+to\s+keep\s+working/i,
    /(?:upgrade(?:\s+your\s+plan)?|add\s+credits)(?:\s+or\s+add\s+credits)?\s+to\s+continue/i,
    /try\s+again\s+(?:at\s+\S+|later)/i,
    /(?:send\s+a\s+request\s+to|notify|contact)\s+your\s+(?:workspace\s+)?(?:admin|owner)/i,
    /workspace\s+owner\s+(?:notified|was\s+already\s+notified)|limit\s+increase\s+(?:requested|was\s+already\s+requested)/i,
    /(?:rate\s+limit|usage\s+limit)s?\s+resets?/i,
    /查看工作空间的使用设置|檢查工作區的用量設定/,
    /联系工作空间所有者|聯絡工作區擁有者/,
    /向工作空间管理员申请提高限额|向工作區管理員申請提高上限/,
    /提高用量限额[，,\s]*以便继续工作|提高用量上限[，,\s]*以繼續作業/,
    /(?:升级套餐|充值(?:额度)?|升級方案|加(?:值|購)點數)[\s\S]{0,16}(?:继续|繼續)/,
    /(?:请在\s*\S+\s*后重试|或在\s*\S+\s*再试|请稍后再试|请稍后重试|再試一次|請稍後再試)/,
    /请向管理员发送请求|已通知(?:过)?管理员|请求已发送给管理员|已通知工作(?:空间所有者|區擁有者)|已申请提高(?:限额|使用上限)/,
    /(?:速率限制|使用额度|用量)[\s\S]{0,40}重置/,
  ];
  const remainingQuotaTextPattern = /(?:剩余\s*\d+\s*%|\b\d+\s*%\s*(?:left|remaining)\b)/i;
  const goalBannerPrefixPattern = /^(?:[^\p{L}\p{N}]*\s*)?(?:(?:已(?:暂停|达成|完成|停止|取消)|进行中|运行中)的目标|目标(?:已(?:暂停|达成|完成|停止|取消|停滞)|使用受限|受限)|目標(?:已(?:暫停|達成|完成|停止|取消)|停滯|使用次數受限|受限)|進行中的目標|(?:paused|active|running|completed|stopped|cancel+ed|stalled)\s+goal|goal\s+(?:paused|active|running|completed|stopped|cancel+ed|stalled|usage\s+limited|budget\s+limited))(?:\s|$)/iu;
  const reactInternalKeyPattern = /^__(?:reactProps|reactFiber)\$/;
  const reactTraversalIgnoredKeys = new Set([
    "return",
    "child",
    "sibling",
    "stateNode",
    "_owner",
    "_store",
    "_source",
    "alternate",
  ]);
  const reactGoalIdPattern = /(?:^|[._:/-])(?:thread[_-]?goals?|goal[_-]?(?:banner|bar|card|pill|row|status|header|strip|tray)|paused[_-]?goal|active[_-]?goal)(?:$|[._:/-])/i;
  const reactNativeDismissIdPattern = /(?:^|[._:/-])(?:(?:rate[_-]?limit|usage[_-]?limit|upsell|quota)[a-z0-9_.-]*[._:/-]dismiss|dismiss[_-]?usage[_-]?banner)$/i;
  const reactTitleIdPattern = /(?:^|[._:/-])(?:localConversation\.usageLimit(?:\.[a-z0-9_.-]+)?|(?:rate[_-]?limit[_-]?)?upsell[_-]?banner(?:\.enterprise\.warning\.title)?|out[_-]?of[_-]?(?:[a-z0-9]+[_-]?)?usage|(?:usage|rate|quota)[_-]?limits?[_-]?(?:reached|exhausted|exceeded|banner|notice|alert|callout|hit)|(?:codex|work|team|workspace)[_-]?(?:and[_-]?work[_-]?)?(?:usage|rate|quota)[_-]?(?:limits?[_-]?)?(?:reached|exhausted|exceeded|out)|rate[_-]?limit[_-]?(?:banner|notice|callout|card)|usage[_-]?limit[_-]?(?:banner|notice|callout|card)|quota[_-]?(?:banner|notice|callout|card))(?:$|[._:/-])/i;
  const reactSupportIdPattern = /(?:^|[._:/-])(?:localConversation\.usageLimit\.[a-z0-9_.-]+|upsellBanner\.(?:enterprise\.warning\.(?:headline|ownerHeadline|limitContext)|notifyOwner\.(?:sent|cooldownActive|failed)|requestIncrease\.(?:sent|cooldownActive|failed))|creditRequest\.(?:success|alreadyRequested|error|requestedTooltip)|(?:rate|usage)[_-]?limit[_-]?(?:reset|resets|renews|status|description|body)|out[_-]?of[_-]?usage[_-]?(?:description|body|subtitle)|request[_-]?more[_-]?usage|(?:contact|ask|request|notify)[_-]?(?:admin|owner))(?:$|[._:/-])/i;
  const reactControlIdPattern = /(?:^|[._:/-])(?:add[_-]?credits|request[_-]?(?:increase|more[_-]?usage)|ownerCta|increase[_-]?usage[_-]?limit|notify[_-]?admin|use[_-]?available[_-]?reset|reset[_-]?(?:usage|rate[_-]?limit|limit)|ask[_-]?admin(?:[_-]?for[_-]?(?:more[_-]?)?usage)?)(?:$|[._:/-])/i;
  const reactActionControlIdPattern = /(?:^|[._:/-])(?:upgrade(?:[_-]?(?:plan|cta|button|now|to[_-]?(?:pro|plus|go)))?|rejoin[_-]?pro|continue[_-]?with[_-]?plus|view[_-]?plans)(?:$|[._:/-])/i;
  const pendingRoots = new Set();
  const pendingRootStates = new WeakMap();
  const dismissedNotices = new Set();
  const reactSignalCache = new WeakMap();
  let unsubscribeMutations = null;
  let scanTimer = 0;
  let disposed = false;
  let scanCount = 0;
  let scannedElements = 0;
  let childExpansions = 0;

  const normalizedValue = (value) => String(value || "")
    .replace(/[\u2018\u2019\u201b\u02bc\uFF07]/g, "'")
    .replace(/\s+/g, " ")
    .trim();
  const normalizedText = (element) => normalizedValue(element?.textContent);
  const matchesAny = (value, patterns) => patterns.some((pattern) => pattern.test(value));
  const directText = (element) => {
    if (!element) return "";
    if (element.childNodes) {
      return normalizedValue([...element.childNodes]
        .filter((node) => node.nodeType === 3 || node.nodeType === 4)
        .map((node) => node.textContent || "")
        .join(" "));
    }
    return element.children?.length ? "" : normalizedValue(element.textContent);
  };

  const reactInternalsFor = (element) => {
    if (!element || typeof element !== "object") return [];
    const shared = window.__codeySharedRuntime?.reactInternals;
    if (typeof shared === "function") {
      try {
        const internals = shared(element);
        if (Array.isArray(internals) && internals.length > 0) return internals;
      } catch {}
    }
    try {
      return Object.keys(element)
        .filter((key) => reactInternalKeyPattern.test(key))
        .map((key) => element[key])
        .filter(Boolean);
    } catch {
      return [];
    }
  };

  const collectElementReactPayloads = (element) => {
    const internals = reactInternalsFor(element);
    if (!internals.length) return [];
    const isControlElement = element.matches?.(controlSelector) === true;
    const isLeafElement = (element.children?.length || 0) === 0;
    const allowChildren = isLeafElement || isControlElement;
    const entries = [];
    const seen = new Set();
    const pushPayload = (value, ignoreChildren = !allowChildren, isComponentName = false) => {
      if (isComponentName) {
        if (typeof value === "string" && value && !seen.has(value)) {
          seen.add(value);
          entries.push({ ignoreChildren: true, isComponentName: true, value });
        }
        return;
      }
      if (value && typeof value === "object" && !seen.has(value)) {
        seen.add(value);
        entries.push({ ignoreChildren, isComponentName: false, value });
      }
    };
    for (const internal of internals) {
      if (!internal || typeof internal !== "object") continue;
      if (internal.memoizedProps || internal.pendingProps || internal.type) {
        pushPayload(internal.memoizedProps ?? internal.pendingProps, !allowChildren);
        const owner = internal.sibling == null ? internal.return : null;
        if (
          owner
          && typeof owner.type === "function"
          && (owner.child == null || owner.child === internal)
        ) {
          pushPayload(owner.memoizedProps ?? owner.pendingProps, true);
          const typeName = owner.type?.displayName || owner.type?.name;
          if (typeof typeName === "string" && typeName) {
            pushPayload(typeName, true, true);
          }
        }
        if (allowChildren) {
          let child = internal.child;
          for (let depth = 0; child && depth < 2 && child.sibling == null; depth += 1) {
            if (typeof child.type === "string" || child.stateNode instanceof Element) break;
            pushPayload(child.memoizedProps ?? child.pendingProps, false);
            child = child.child;
          }
        }
      } else {
        pushPayload(internal, !allowChildren);
      }
    }
    return entries;
  };

  const hasQuotaResetCallbackProp = (obj) => (
    typeof obj.onResetUsage === "function"
    || typeof obj.onResetRateLimit === "function"
    || typeof obj.onRequestMoreUsage === "function"
    || typeof obj.onRequestIncrease === "function"
    || typeof obj.onNotifyAdmin === "function"
    || typeof obj.onAddCredits === "function"
  );

  const hasQuotaUpgradeCallbackProp = (obj) => (
    typeof obj.onUpgrade === "function"
    || typeof obj.onUpgradePlan === "function"
  );

  const hasQuotaResetStateProp = (obj) => (
    obj.resetsAt != null
    || obj.resetAt != null
    || obj.resetDate != null
    || obj.rateLimitResetAt != null
    || obj.rate_limit_reset_at != null
    || obj.rate_limit_reset_credits != null
  );

  const hasQuotaExhaustedFlagProp = (obj) => (
    obj.isOutOfUsage === true
    || obj.outOfUsage === true
    || obj.usageLimitReached === true
    || obj.isUsageLimitReached === true
    || obj.rateLimitReached === true
    || obj.isRateLimitReached === true
    || obj.isRateLimited === true
    || obj.isQuotaExhausted === true
    || obj.quotaExhausted === true
    || (typeof obj.rateLimitReachedType === "string" && obj.rateLimitReachedType.length > 0)
    || (typeof obj.rate_limit_reached_type === "string" && obj.rate_limit_reached_type.length > 0)
  );

  const inspectElementReactSignals = (element) => {
    if (!(element instanceof Element)) {
      return { actionControl: false, control: false, goal: false, nativeDismiss: false, support: false, title: false };
    }
    const payloadEntries = collectElementReactPayloads(element);
    if (!payloadEntries.length) {
      return { actionControl: false, control: false, goal: false, nativeDismiss: false, support: false, title: false };
    }
    const payloads = payloadEntries.map((entry) => entry.value);
    const cached = reactSignalCache.get(element);
    if (
      cached
      && cached.payloads.length === payloads.length
      && cached.payloads.every((payload, index) => payload === payloads[index])
    ) {
      return cached.signals;
    }
    const isControlElement = element.matches?.(controlSelector) === true;
    const signals = {
      actionControl: false,
      control: false,
      goal: false,
      nativeDismiss: false,
      support: false,
      title: false,
    };
    const visit = (current, depth, keyHint = "", ignoreChildren = false, seen = new WeakSet()) => {
      if (typeof current === "string") {
        const text = normalizedValue(current);
        if (!text) return;
        if (
          keyHint === "id"
          || keyHint === "messageId"
          || keyHint === "testId"
          || keyHint === "data-testid"
          || keyHint === "__componentName"
        ) {
          if (reactGoalIdPattern.test(text)) signals.goal = true;
          if (reactNativeDismissIdPattern.test(text)) {
            signals.nativeDismiss = true;
            return;
          }
          if (reactControlIdPattern.test(text)) {
            signals.control = true;
            signals.actionControl = true;
            return;
          }
          if (reactActionControlIdPattern.test(text)) {
            signals.actionControl = true;
            return;
          }
          if (reactTitleIdPattern.test(text)) signals.title = true;
          if (reactSupportIdPattern.test(text)) signals.support = true;
        }
        if (
          keyHint === "defaultMessage"
          || keyHint === "title"
          || keyHint === "description"
          || keyHint === "label"
          || keyHint === "text"
          || keyHint === "aria-label"
        ) {
          if (goalBannerPrefixPattern.test(text)) signals.goal = true;
          if (matchesAny(text, nativeDismissLabels)) {
            signals.nativeDismiss = true;
            return;
          }
          if (matchesAny(text, titlePatterns)) signals.title = true;
          if (matchesAny(text, noticeSupportPatterns)) signals.support = true;
          if (matchesAny(text, controlLabels)) {
            signals.control = true;
            signals.actionControl = true;
          }
          if (matchesAny(text, actionControlLabels)) {
            signals.actionControl = true;
          }
        }
        return;
      }
      if (!current || typeof current !== "object" || depth > 5 || seen.has(current)) return;
      seen.add(current);
      if (hasQuotaExhaustedFlagProp(current)) signals.title = true;
      if (hasQuotaResetCallbackProp(current)) {
        signals.control = true;
        signals.actionControl = true;
      }
      if (hasQuotaUpgradeCallbackProp(current)) {
        signals.actionControl = true;
      }
      if (hasQuotaResetStateProp(current)) {
        signals.support = true;
        if (
          !isControlElement
          && (
            hasQuotaResetCallbackProp(current)
            || hasQuotaUpgradeCallbackProp(current)
            || hasQuotaExhaustedFlagProp(current)
          )
        ) {
          signals.title = true;
        }
      }
      let entries;
      try {
        entries = Object.entries(current);
      } catch {
        return;
      }
      for (const [key, child] of entries) {
        if (reactTraversalIgnoredKeys.has(key)) continue;
        if (ignoreChildren && key === "children") continue;
        visit(child, depth + 1, key, ignoreChildren, seen);
      }
    };
    for (const entry of payloadEntries) {
      if (entry.isComponentName) {
        visit(entry.value, 0, "__componentName", true);
      } else {
        visit(entry.value, 0, "", entry.ignoreChildren);
      }
    }
    reactSignalCache.set(element, { payloads, signals });
    return signals;
  };

  const isOwnGoalElement = (element, precomputedReact = null) => {
    if (!(element instanceof Element)) return false;
    if ((precomputedReact || inspectElementReactSignals(element)).goal) return true;
    if (controlAttributeCandidates(element).some((attr) => reactGoalIdPattern.test(attr))) {
      return true;
    }
    const direct = directText(element);
    if (direct && goalBannerPrefixPattern.test(direct)) return true;
    return [...(element.children || [])].some((child) => (
      (child.children?.length || 0) === 0
      && goalBannerPrefixPattern.test(directText(child))
    ));
  };

  const subtreeReactSignals = (element, cache) => {
    const cached = cache.get(element);
    if (cached) return cached;
    const own = inspectElementReactSignals(element);
    let goal = isOwnGoalElement(element, own);
    let title = own.title;
    let support = own.support;
    const stack = [...(element.children || [])];
    let visited = 0;
    while (stack.length > 0 && visited < 48 && !goal) {
      const node = stack.pop();
      if (!(node instanceof Element)) continue;
      visited += 1;
      const childOwn = inspectElementReactSignals(node);
      if (isOwnGoalElement(node, childOwn)) {
        goal = true;
        break;
      }
      if (childOwn.title) title = true;
      if (childOwn.support) support = true;
      if (node.children?.length) {
        for (const child of node.children) stack.push(child);
      }
    }
    const result = { goal, support, title };
    cache.set(element, result);
    return result;
  };

  const controlAttributeCandidates = (control) => [
    control?.getAttribute?.("data-testid"),
    control?.getAttribute?.("data-action"),
    control?.getAttribute?.("id"),
    control?.getAttribute?.("name"),
  ].map(normalizedValue).filter(Boolean);

  const controlLabelCandidates = (control) => [
    control?.getAttribute?.("aria-label"),
    control?.getAttribute?.("title"),
    control?.textContent,
  ].map(normalizedValue).filter(Boolean);

  const isNoticeControl = (control, precomputedReact = null) => (
    control instanceof Element
    && control.matches?.(controlSelector)
    && control.getAttribute?.(closeAttribute) !== "true"
    && !control.closest?.(`[${dismissedAttribute}="true"]`)
    && (
      (precomputedReact || inspectElementReactSignals(control)).control
      || controlAttributeCandidates(control).some((attr) => reactControlIdPattern.test(attr))
      || controlLabelCandidates(control).some((label) => matchesAny(label, controlLabels))
    )
  );

  const isNoticeActionControl = (control, precomputedReact = null) => {
    if (
      !(control instanceof Element)
      || !control.matches?.(controlSelector)
      || control.getAttribute?.(closeAttribute) === "true"
      || control.closest?.(`[${dismissedAttribute}="true"]`)
    ) {
      return false;
    }
    const react = precomputedReact || inspectElementReactSignals(control);
    if (isNoticeControl(control, react)) return true;
    return (
      react.actionControl
      || controlAttributeCandidates(control).some((attr) => reactActionControlIdPattern.test(attr))
      || controlLabelCandidates(control).some((label) => matchesAny(label, actionControlLabels))
    );
  };

  const shouldSkipMountedTurnMutation = (mutation, target, isNotice) => {
    if (
      (mutation.type !== "childList" && mutation.type !== "characterData")
      || !target?.closest?.(conversationTurnSelector)
    ) {
      return false;
    }
    const targetSurface = target.closest?.(warningSurfaceSelector);
    const targetPortal = target.closest?.("[data-radix-portal], [data-portal]");
    if (targetSurface && targetSurface !== targetPortal) {
      return false;
    }
    if (targetPortal && isNotice(targetPortal)) return false;
    return ![...(mutation.addedNodes || [])].some((node) => (
      node instanceof Element
      && (
        node.matches?.(warningSurfaceSelector)
        || [...(node.children || [])].some((child) => (
          child.matches?.(warningSurfaceSelector)
          || child.querySelector?.(warningSurfaceSelector)
        ))
      )
    ));
  };

  const isNativeDismissControl = (control, precomputedReact = null) => {
    if (
      !(control instanceof Element)
      || !control.matches?.("button, [role=button]")
      || control.getAttribute?.(closeAttribute) === "true"
      || control.disabled
      || control.closest?.(`[${dismissedAttribute}="true"]`)
    ) {
      return false;
    }
    const react = precomputedReact || inspectElementReactSignals(control);
    return (
      react.nativeDismiss
      || controlAttributeCandidates(control).some((attr) => reactNativeDismissIdPattern.test(attr))
      || controlLabelCandidates(control).some((label) => matchesAny(label, nativeDismissLabels))
    );
  };

  const hasNativeDismissControl = (element) => (
    typeof element?.querySelectorAll === "function"
    && [...element.querySelectorAll("button, [role=button]")].some((control) => isNativeDismissControl(control))
  );

  const hasNoticeControl = (element) => (
    typeof element?.querySelectorAll === "function"
    && [...element.querySelectorAll(controlSelector)].some(isNoticeControl)
  );

  const hasNoticeActionControl = (element) => (
    typeof element?.querySelectorAll === "function"
    && [...element.querySelectorAll(controlSelector)].some(isNoticeActionControl)
  );

  const composerStopSelector = [
    "form",
    "textarea",
    "[contenteditable='true']",
    "[contenteditable='plaintext-only']",
    "[role='textbox']",
  ].join(", ");
  const sidebarStopSelector = [
    "[data-app-action-sidebar-section]",
    "[data-app-action-sidebar-thread-row]",
    "[data-app-action-sidebar-project-row]",
    "#codey-account-usage",
  ].join(", ");
  const surfaceStopSelector = [
    "main",
    "[role='main']",
    "nav",
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    composerStopSelector,
    conversationTurnSelector,
  ].join(", ");

  const nonNoticeSurfaceSelector = [
    "main",
    "[role='main']",
    "nav",
    composerStopSelector,
    conversationTurnSelector,
    sidebarStopSelector,
  ].join(", ");

  // 额度耗尽提示固定挂载在底部输入区容器内、输入框正上方的同级横幅槽位。
  const isComposerBannerSlot = (element) => {
    if (
      !(element instanceof Element)
      || element === document.body
      || element === document.documentElement
      || element.matches?.(surfaceStopSelector)
      || element.closest?.(conversationTurnSelector)
      || element.closest?.(sidebarStopSelector)
      || element.querySelector?.(composerStopSelector)
      || element.querySelector?.(conversationTurnSelector)
    ) {
      return false;
    }
    let parent = element.parentElement;
    for (let depth = 0; parent && depth < 3; depth += 1) {
      if (
        parent === document.body
        || parent === document.documentElement
        || parent.matches?.("main, [role='main'], nav")
      ) {
        break;
      }
      if (parent.querySelector?.(composerStopSelector)) return true;
      parent = parent.parentElement;
    }
    return false;
  };

  const createNoticeDetector = () => {
    const noticeControlCache = new WeakMap();
    const actionControlCache = new WeakMap();
    const nativeDismissCache = new WeakMap();
    const reactSubtreeCache = new WeakMap();
    const textCache = new WeakMap();
    const readText = (element) => {
      let text = textCache.get(element);
      if (text === undefined) {
        text = normalizedText(element);
        textCache.set(element, text);
      }
      return text;
    };
    const readHasControl = (element) => {
      let hasControl = noticeControlCache.get(element);
      if (hasControl === undefined) {
        hasControl = hasNoticeControl(element);
        noticeControlCache.set(element, hasControl);
      }
      return hasControl;
    };
    const readHasActionControl = (element) => {
      let hasAction = actionControlCache.get(element);
      if (hasAction === undefined) {
        hasAction = hasNoticeActionControl(element);
        actionControlCache.set(element, hasAction);
      }
      return hasAction;
    };
    const readHasNativeDismiss = (element) => {
      let hasDismiss = nativeDismissCache.get(element);
      if (hasDismiss === undefined) {
        hasDismiss = hasNativeDismissControl(element);
        nativeDismissCache.set(element, hasDismiss);
      }
      return hasDismiss;
    };
    return (element) => {
      if (!(element instanceof Element)) return false;
      if (element === document.body || element === document.documentElement) return false;
      if (element.matches?.(controlSelector) || element.matches?.(nonNoticeSurfaceSelector)) {
        return false;
      }
      if (element.getAttribute?.(dismissedAttribute) === "true" || dismissedNotices.has(element)) {
        return false;
      }
      if (
        element.closest?.(sidebarStopSelector)
        || element.querySelector?.(composerStopSelector)
        || element.querySelector?.(conversationTurnSelector)
        || element.querySelector?.(`[${dismissedAttribute}="true"]`)
      ) {
        return false;
      }
      const reactSignals = subtreeReactSignals(element, reactSubtreeCache);
      if (reactSignals.goal) return false;
      if (readHasNativeDismiss(element)) return false;
      if (element.getAttribute?.(noticeAttribute) === "true") return true;
      if (element.querySelector?.(`[${noticeAttribute}="true"]`)) return false;
      const text = readText(element);
      if (goalBannerPrefixPattern.test(text)) return false;
      const hasTitle = reactSignals.title || matchesAny(text, titlePatterns);
      const hasSupport = reactSignals.support || matchesAny(text, noticeSupportPatterns);
      if (hasTitle) {
        return hasSupport || readHasControl(element) || readHasActionControl(element);
      }
      if (
        isComposerBannerSlot(element)
        && !remainingQuotaTextPattern.test(text)
        && readHasControl(element)
        && hasSupport
      ) {
        return true;
      }
      return false;
    };
  };

  const isQuotaNotice = (element) => createNoticeDetector()(element);

  const createContinuation = (root, continuation = null) => (
    window.__codeySharedRuntime.createElementContinuation(root, continuation)
  );
  const hasContinuation = (continuation) => (
    window.__codeySharedRuntime.hasElementContinuation(continuation)
  );

  const anchorsWithin = (root, { continuation = null, remaining = null } = {}) => {
    const titleAnchors = [];
    const noticeControls = [];
    const actionControls = [];
    const traversal = window.__codeySharedRuntime.walkElementsBudgeted(
      root,
      (element) => {
        const dismissedTarget = element.getAttribute?.(dismissedAttribute) === "true"
          || dismissedNotices.has(element)
          ? element
          : element.closest?.(`[${dismissedAttribute}="true"]`);
        if (dismissedTarget) {
          hideNotice(dismissedTarget);
          return false;
        }
        const ownReact = inspectElementReactSignals(element);
        if (isOwnGoalElement(element, ownReact)) {
          element.removeAttribute?.(noticeAttribute);
          element.querySelector?.(`[${closeAttribute}="true"]`)?.remove?.();
          return false;
        }
        if (isNativeDismissControl(element, ownReact)) {
          const decorated = element.closest?.(`[${noticeAttribute}="true"]`);
          if (decorated) {
            decorated.removeAttribute?.(noticeAttribute);
            noticeSurface(decorated)?.querySelector?.(`[${closeAttribute}="true"]`)?.remove?.();
          }
          return;
        }
        if (
          ownReact.title
          || element.getAttribute?.(noticeAttribute) === "true"
          || matchesAny(directText(element), titlePatterns)
        ) {
          titleAnchors.push(element);
        }
        if (isNoticeControl(element, ownReact)) noticeControls.push(element);
        else if (isNoticeActionControl(element, ownReact)) actionControls.push(element);
      },
      { continuation, remaining },
    );
    scannedElements += traversal.visited;
    childExpansions += traversal.childExpansions;
    return {
      actionControls,
      continuation: traversal.continuation,
      noticeControls,
      titleAnchors,
    };
  };

  const nearestNotice = (start, isNotice = isQuotaNotice) => {
    let candidate = start instanceof Element ? start : start?.parentElement;
    for (let depth = 0; candidate && depth < 12; depth += 1) {
      if (candidate === document.body || candidate === document.documentElement) break;
      if (isNotice(candidate)) return candidate;
      candidate = candidate.parentElement;
    }
    return null;
  };

  const innermostNotices = (notices) => {
    const candidates = [...new Set(notices)].filter(Boolean);
    return candidates.filter((candidate) => !candidates.some((other) => (
      other !== candidate && candidate.contains?.(other)
    )));
  };

  const resolveNotice = (start) => {
    const isNotice = createNoticeDetector();
    const notice = nearestNotice(start, isNotice);
    const searchRoot = notice || (start instanceof Element ? start : null);
    if (!searchRoot) return null;
    const {
      actionControls,
      noticeControls,
      titleAnchors,
    } = anchorsWithin(searchRoot);
    const nested = innermostNotices(
      [...titleAnchors, ...noticeControls, ...actionControls]
        .map((anchor) => nearestNotice(anchor, isNotice))
        .filter(Boolean),
    );
    const target = nested[0] || notice;
    return target ? completeNotice(target, isNotice) : null;
  };

  // 标题和说明可能单独成块，重置、申请额度或升级按钮在兄弟容器里。
  // 关闭按钮应放在包含这些操作的完整提示框上。
  const canExpandNoticeTo = (candidate, isNotice) => (
    !candidate.matches?.(surfaceStopSelector)
    && !candidate.querySelector?.(composerStopSelector)
    && isNotice(candidate)
  );
  const completeNotice = (resolved, isNotice) => {
    if (!resolved || hasNoticeControl(resolved) || hasNoticeActionControl(resolved)) {
      return resolved;
    }
    let candidate = resolved.parentElement;
    for (let depth = 0; candidate && depth < 2; depth += 1) {
      if (candidate === document.body || candidate === document.documentElement) break;
      if (
        canExpandNoticeTo(candidate, isNotice)
        && (hasNoticeControl(candidate) || hasNoticeActionControl(candidate))
      ) {
        return candidate;
      }
      candidate = candidate.parentElement;
    }
    return resolved;
  };
  const ignorableShellSibling = (element) => (
    element instanceof Element
    && (
      element.getAttribute?.(closeAttribute) === "true"
      || ["SCRIPT", "STYLE", "TEMPLATE", "NOSCRIPT", "LINK", "META", "BR", "HR"]
        .includes(element.tagName)
      || (
        ["DIV", "SPAN", "SECTION", "ASIDE", "P"].includes(element.tagName)
        && element.children.length === 0
        && !directText(element)
      )
    )
  );
  // 已定位的浮层本身就是可见表面，停在它身上，不并入外面的纯包裹。
  const positionedSurfaceSelector = [
    "dialog",
    "[role='dialog']",
    "[role='alertdialog']",
    "[aria-modal='true']",
    "[data-radix-portal]",
    "[data-portal]",
  ].join(", ");
  const noticeSurface = (resolved) => {
    let surface = resolved;
    for (let depth = 0; depth < 12; depth += 1) {
      if (surface?.matches?.(positionedSurfaceSelector)) break;
      const parent = surface?.parentElement;
      if (!(parent instanceof Element)
        || parent === document.body
        || parent === document.documentElement
        || parent.matches?.(surfaceStopSelector)
        || directText(parent)) break;
      if ([...(parent.children || [])].some((child) => (
        child !== surface && !ignorableShellSibling(child)
      ))) break;
      surface = parent;
    }
    return surface || resolved;
  };

  const addStyle = () => {
    if (document.getElementById?.(styleId)) return;
    const parent = document.head || document.documentElement;
    if (!parent || typeof document.createElement !== "function") return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      button[${closeAttribute}="true"] {
        all: unset !important;
        position: absolute !important;
        z-index: 6 !important;
        inset-block-start: -8px !important;
        inset-inline-end: -8px !important;
        display: grid !important;
        place-items: center !important;
        box-sizing: border-box !important;
        width: 30px !important;
        height: 30px !important;
        padding: 0 !important;
        border: 1px solid light-dark(rgba(0, 0, 0, .10), rgba(255, 255, 255, .16)) !important;
        border-radius: 999px !important;
        background: light-dark(#ffffff, #2d2f33) !important;
        color: light-dark(#55575c, #ececef) !important;
        box-shadow: 0 2px 8px light-dark(rgba(27, 31, 38, .12), rgba(0, 0, 0, .34)) !important;
        color-scheme: light dark !important;
        cursor: pointer !important;
        font: 500 21px/1 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif !important;
        pointer-events: auto !important;
        touch-action: manipulation !important;
        user-select: none !important;
        transition: background-color .14s ease, border-color .14s ease, transform .14s ease !important;
        -webkit-app-region: no-drag !important;
      }
      button[${closeAttribute}="true"]:hover {
        background: light-dark(#f2f3f5, #3a3d42) !important;
        border-color: light-dark(rgba(0, 0, 0, .17), rgba(255, 255, 255, .24)) !important;
      }
      button[${closeAttribute}="true"]:active {
        transform: scale(.94) !important;
      }
      button[${closeAttribute}="true"]:focus-visible {
        outline: 2px solid light-dark(#0a84ff, #69b1ff) !important;
        outline-offset: 2px !important;
      }
      @media (max-width: 520px) {
        button[${closeAttribute}="true"] {
          inset-block-start: -7px !important;
          inset-inline-end: -7px !important;
          width: 28px !important;
          height: 28px !important;
          font-size: 19px !important;
        }
      }
    `;
    parent.appendChild(style);
  };

  const hideNotice = (notice) => {
    if (!notice) return false;
    const surface = noticeSurface(notice);
    dismissedNotices.add(notice);
    dismissedNotices.add(surface);
    surface.querySelector?.(`[${closeAttribute}="true"]`)?.remove?.();
    notice.setAttribute?.(noticeAttribute, "true");
    notice.setAttribute?.(dismissedAttribute, "true");
    surface.setAttribute?.(dismissedAttribute, "true");
    surface.setAttribute?.("aria-hidden", "true");
    surface.style?.setProperty?.("display", "none", "important");
    return true;
  };

  const dismiss = (notice) => {
    const resolved = resolveNotice(notice);
    return resolved ? hideNotice(resolved) : false;
  };

  const ensureCloseButton = (notice) => {
    const surface = noticeSurface(notice);
    if (dismissedNotices.has(notice) || dismissedNotices.has(surface)) {
      hideNotice(notice);
      return null;
    }
    if (hasNativeDismissControl(notice) || (surface !== notice && hasNativeDismissControl(surface))) {
      notice.removeAttribute?.(noticeAttribute);
      surface.querySelector?.(`[${closeAttribute}="true"]`)?.remove?.();
      return null;
    }
    const existing = surface.querySelector?.(`[${closeAttribute}="true"]`);
    if (existing) return existing;

    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute("type", "button");
    button.setAttribute(closeAttribute, "true");
    button.setAttribute("aria-label", "关闭使用额度提示");
    button.setAttribute("title", "关闭使用额度提示");
    button.textContent = "\u00d7";
    button.addEventListener("click", () => {
      hideNotice(notice);
    });
    notice.setAttribute?.(noticeAttribute, "true");
    const computedPosition = window.getComputedStyle?.(surface)?.position;
    if (!computedPosition || computedPosition === "static") {
      surface.style?.setProperty?.("position", "relative", "important");
    }
    surface.insertBefore?.(button, surface.firstChild || null);
    return button;
  };

  const scanRoot = (root, {
    continuation = null,
    detector = createNoticeDetector(),
    remaining = null,
  } = {}) => {
    if (disposed || !root) return { continuation: { segments: [] }, found: 0 };
    scanCount += 1;
    addStyle();
    const {
      actionControls,
      continuation: nextContinuation,
      noticeControls,
      titleAnchors,
    } = anchorsWithin(root, { continuation, remaining });
    const starts = [
      ...titleAnchors,
      ...noticeControls,
      ...actionControls,
    ];
    const matches = innermostNotices(
      starts.map((start) => nearestNotice(start, detector)).filter(Boolean),
    );
    let found = 0;
    for (const notice of matches) {
      const resolved = completeNotice(notice, detector);
      if (resolved.isConnected === false) continue;
      if (dismissedNotices.has(resolved)) {
        hideNotice(resolved);
        continue;
      }
      if (ensureCloseButton(resolved)) found += 1;
    }
    return {
      continuation: nextContinuation,
      found,
    };
  };

  const scan = (root = document, { bounded = false } = {}) => {
    if (disposed || !root) return 0;
    return scanRoot(root, {
      remaining: bounded ? { value: batchElementLimit } : null,
    }).found;
  };

  const mergeContinuations = (...continuations) => {
    const segments = [];
    const seenElements = new Set();
    for (const continuation of continuations) {
      for (const segment of continuation?.segments || []) {
        if (!segment.some((frame) => !seenElements.has(frame.element))) continue;
        for (const frame of segment) seenElements.add(frame.element);
        segments.push(segment);
      }
    }
    return { segments };
  };

  const continuationHasElement = (continuation, element) => (
    continuation?.segments?.some?.((segment) => (
      segment.some((frame) => frame.element === element)
    )) === true
  );

  const addPendingRoot = (root) => {
    if (!(root instanceof Element) || root.isConnected === false) return;
    for (const pending of pendingRoots) {
      if (pending === root || pending.contains?.(root)) {
        // A nested change under a partially scanned root may sit after the
        // ancestor's saved traversal position. Add that child as a resumed
        // root instead of dropping the mutation or restarting from the top.
        const continuation = pendingRootStates.get(pending);
        if (hasContinuation(continuation)) {
          pendingRootStates.set(
            pending,
            pending === root || continuationHasElement(continuation, root)
              ? mergeContinuations(
                  continuation,
                  createContinuation(root, continuation),
                )
              : mergeContinuations(
                  createContinuation(root),
                  continuation,
                ),
          );
        }
        return;
      }
    }
    let mergedContinuation = pendingRootStates.get(root) || pendingRootStates.get(
      [...pendingRoots].find((pending) => pending.contains?.(root)),
    ) || { segments: [] };
    for (const pending of [...pendingRoots]) {
      if (root.contains?.(pending)) {
        mergedContinuation = mergeContinuations(
          pendingRootStates.get(pending) || createContinuation(pending),
          mergedContinuation,
        );
        pendingRoots.delete(pending);
        pendingRootStates.delete(pending);
      }
    }
    if (pendingRoots.size >= scanningRootLimit) {
      // 新版节点仍应优先于旧节点。绝不在这里退化为整页扫描，流式输出会因此
      // 反复构造页面文本并阻塞主线程。
      const evicted = pendingRoots.values().next().value;
      pendingRoots.delete(evicted);
      pendingRootStates.delete(evicted);
    }
    pendingRoots.add(root);
    if (hasContinuation(mergedContinuation)) {
      pendingRootStates.set(root, mergedContinuation);
    }
  };

  const scheduleScan = () => {
    if (disposed || scanTimer || !pendingRoots.size) return;
    scanTimer = window.setTimeout(() => {
      scanTimer = 0;
      const roots = [...pendingRoots];
      pendingRoots.clear();
      const detector = createNoticeDetector();
      const remaining = { value: batchElementLimit };
      for (let index = 0; index < roots.length; index += 1) {
        const root = roots[index];
        if (root.isConnected === false) {
          pendingRootStates.delete(root);
          continue;
        }
        if (remaining.value <= 0) {
          roots.slice(index).forEach(addPendingRoot);
          break;
        }
        const { continuation } = scanRoot(root, {
          continuation: pendingRootStates.get(root),
          detector,
          remaining,
        });
        if (hasContinuation(continuation)) {
          pendingRootStates.set(root, continuation);
          roots.slice(index + 1).forEach(addPendingRoot);
          addPendingRoot(root);
          break;
        }
        pendingRootStates.delete(root);
      }
      if (pendingRoots.size) scheduleScan();
    }, scanDelayMs);
  };

  const pruneRemovedNotices = (removedNodes) => {
    if (!dismissedNotices.size || !removedNodes?.length) return;
    for (const notice of [...dismissedNotices]) {
      if (
        notice.isConnected === false
        || [...removedNodes].some((node) => node === notice || node?.contains?.(notice))
      ) {
        dismissedNotices.delete(notice);
      }
    }
  };

  const handleMutations = (mutations) => {
    if (disposed) return;
    let changed = false;
    const detector = createNoticeDetector();
    for (const mutation of mutations) {
      pruneRemovedNotices(mutation.removedNodes);
      const target = mutation.target instanceof Element
        ? mutation.target
        : mutation.target?.parentElement;
      if (shouldSkipMountedTurnMutation(mutation, target, detector)) continue;
      if (mutation.type === "attributes" && target) {
        addPendingRoot(target);
        changed = true;
      }
      let addedText = false;
      for (const node of mutation.addedNodes || []) {
        if (node instanceof Element) {
          addPendingRoot(node);
          changed = true;
        } else if (node?.nodeType === 3 || node?.nodeType === 4) {
          addedText = true;
        }
      }
      if (addedText && target) {
        addPendingRoot(target);
        changed = true;
      }
    }
    if (changed) scheduleScan();
  };

  const ensureObserver = () => {
    if (
      disposed
      || unsubscribeMutations
      || typeof window.__codeyMutationDispatcher?.subscribe !== "function"
      || !document.documentElement
    ) return;
    unsubscribeMutations = window.__codeyMutationDispatcher.subscribe(
      handleMutations,
      {
        attributes: true,
        attributeFilter: ["aria-label", "title"],
        childList: true,
      },
    );
  };

  const dispose = () => {
    if (disposed) return;
    disposed = true;
    if (scanTimer) window.clearTimeout?.(scanTimer);
    scanTimer = 0;
    pendingRoots.clear();
    dismissedNotices.clear();
    unsubscribeMutations?.();
    unsubscribeMutations = null;
  };

  window.__codeyQuotaNoticeDismiss = Object.freeze({
    dismiss,
    dispose,
    isQuotaNotice,
    scan,
    snapshot: () => Object.freeze({
      dismissedNotices: dismissedNotices.size,
      batchElementLimit,
      childExpansions,
      observerInstalled: Boolean(unsubscribeMutations),
      pendingRoots: pendingRoots.size,
      scanCount,
      scannedElements,
    }),
  });

  addStyle();
  ensureObserver();
  addPendingRoot(document.documentElement);
  scheduleScan();
})();
