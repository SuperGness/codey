export class FakeElementCore {
  constructor(tagName = "div", { attributes = {}, connected = false } = {}) {
    this.attributes = new Map(Object.entries(attributes));
    this.children = [];
    this.dataset = {};
    this.disabled = false;
    this.id = "";
    this.isConnected = connected;
    this.listeners = new Map();
    this.parentElement = null;
    this.tagName = String(tagName).toUpperCase();
    this.textContent = "";
    const styleValues = new Map();
    this.style = {
      getPropertyValue: (name) => styleValues.get(name) || "",
      removeProperty: (name) => {
        styleValues.delete(name);
        delete this.style[name];
      },
      setProperty: (name, value, priority = "") => {
        const rendered = priority ? `${value}:${priority}` : String(value);
        styleValues.set(name, rendered);
        this.style[name] = rendered;
      },
    };
  }

  get nextElementSibling() {
    if (!this.parentElement) return null;
    const index = this.parentElement.children.indexOf(this);
    return index >= 0 ? this.parentElement.children[index + 1] || null : null;
  }

  addEventListener(type, handler) {
    if (typeof handler !== "function") return;
    const handlers = this.listeners.get(type) || [];
    handlers.push(handler);
    this.listeners.set(type, handlers);
  }

  removeEventListener(type, handler) {
    const handlers = this.listeners.get(type) || [];
    this.listeners.set(type, handlers.filter((candidate) => candidate !== handler));
  }

  dispatchEvent(event) {
    if (!event?.type) return true;
    if (!event.target) event.target = this;
    event.currentTarget = this;
    for (const handler of [...(this.listeners.get(event.type) || [])]) {
      handler.call(this, event);
    }
    return true;
  }

  appendChild(child) {
    child.remove?.();
    child.parentElement = this;
    child.isConnected = true;
    this.children.push(child);
    return child;
  }

  append(...children) {
    children.forEach((child) => this.appendChild(child));
  }

  insertAdjacentElement(position, element) {
    if (position !== "beforebegin" && position !== "afterend") {
      throw new Error(`unsupported insertAdjacentElement position: ${position}`);
    }
    const siblings = this.parentElement.children;
    const index = siblings.indexOf(this);
    element.remove();
    element.parentElement = this.parentElement;
    element.isConnected = this.isConnected;
    siblings.splice(position === "beforebegin" ? index : index + 1, 0, element);
    return element;
  }

  focus() {}

  insertBefore(child, reference) {
    child.remove?.();
    const index = this.children.indexOf(reference);
    if (index < 0) return this.appendChild(child);
    child.parentElement = this;
    child.isConnected = true;
    this.children.splice(index, 0, child);
    return child;
  }

  contains(node) {
    if (node === this) return true;
    return this.children.some((child) => child.contains?.(node));
  }

  getAttribute(name) {
    if (name === "id" && this.id) return this.id;
    return this.attributes.get(name) ?? null;
  }

  hasAttribute(name) {
    return (name === "id" && Boolean(this.id)) || this.attributes.has(name);
  }

  removeAttribute(name) {
    this.attributes.delete(name);
    if (name === "id") this.id = "";
  }

  setAttribute(name, value) {
    const normalized = String(value);
    this.attributes.set(name, normalized);
    if (name === "id") this.id = normalized;
  }

  matches(selector) {
    const candidate = String(selector).trim();
    if (!candidate) return false;
    const alternatives = candidate.split(",").map((part) => part.trim()).filter(Boolean);
    if (alternatives.length > 1) {
      return alternatives.some((part) => this.matches(part));
    }
    if (candidate.startsWith("#")) return this.id === candidate.slice(1);
    const classContains = candidate.match(/^\[class\*=(['"]?)([^\]'"]+)\1\]$/)?.[2];
    if (classContains) return String(this.className || "").includes(classContains);
    const tag = candidate.match(/^[a-z]+/i)?.[0];
    if (tag && this.tagName !== tag.toUpperCase()) return false;
    const attributes = [
      ...candidate.matchAll(/\[([^\]=\]]+)(?:=(?:"([^"]*)"|'([^']*)'|([^\]]+)))?\]/g),
    ];
    if (attributes.length > 0) {
      return attributes.every((match) => {
        if (!this.hasAttribute(match[1])) return false;
        const expected = match[2] ?? match[3] ?? match[4];
        return expected === undefined || this.getAttribute(match[1]) === expected;
      });
    }
    return Boolean(tag);
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] || null;
  }

  querySelectorAll(selector) {
    const selectors = String(selector).split(",").map((candidate) => candidate.trim());
    const matches = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (selectors.some((candidate) => child.matches(candidate))) matches.push(child);
        visit(child);
      }
    };
    visit(this);
    return matches;
  }

  closest(selector) {
    const selectors = String(selector).split(",").map((candidate) => candidate.trim());
    let current = this;
    while (current) {
      if (selectors.some((candidate) => current.matches(candidate))) return current;
      current = current.parentElement;
    }
    return null;
  }

  remove() {
    if (!this.parentElement) return;
    const siblings = this.parentElement.children;
    const index = siblings.indexOf(this);
    if (index >= 0) siblings.splice(index, 1);
    this.parentElement = null;
    this.isConnected = false;
  }
}
