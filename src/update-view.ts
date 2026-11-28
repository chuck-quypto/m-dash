const elementId = (node: ChildNode) => node instanceof Element ? node.id : undefined;

// IDs anchor persistent regions when a refresh inserts or removes their preceding siblings.
// Matched nodes stay connected, so the browser keeps focus, selection and dirty input values.
function updateChildren(parent: Element, next: ParentNode) {
  let current = parent.firstChild;
  function removeUntil(end: ChildNode | null) {
    while (current && current !== end) {
      const old = current;
      current = old.nextSibling;
      old.remove();
    }
  }
  for (const incoming of Array.from(next.childNodes)) {
    const id = elementId(incoming);
    let match = current;
    if (id) {
      while (match && elementId(match) !== id) match = match.nextSibling;
    }
    if (!match || match.nodeName !== incoming.nodeName || elementId(match) !== id) {
      parent.insertBefore(incoming, current);
      continue;
    }
    removeUntil(match);
    current = match.nextSibling;
    if (match.isEqualNode(incoming)) continue;
    if (match instanceof Element && incoming instanceof Element) {
      // Updating options can close a native picker while the user is choosing a value.
      if (match.localName === 'select' && match === parent.ownerDocument.activeElement) continue;
      for (const name of match.getAttributeNames()) {
        if (!incoming.hasAttribute(name) && !(match.localName === 'details' && name === 'open')) match.removeAttribute(name);
      }
      for (const { name, value } of Array.from(incoming.attributes)) {
        if (match.getAttribute(name) !== value) match.setAttribute(name, value);
      }
      updateChildren(match, incoming);
    } else {
      match.nodeValue = incoming.nodeValue;
    }
  }
  removeUntil(null);
}

export function updateView(parent: Element, html: string) {
  const template = parent.ownerDocument.createElement('template');
  template.innerHTML = html;
  updateChildren(parent, template.content);
}
