/** Bare-letter shortcuts must not fire while the user is typing anywhere
 *  editable — task 005's own wording for the plan textarea, generalised to
 *  every editable surface. One definition, shared by the board and the review
 *  view, so the two cannot drift on what counts as a typing surface. */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.tagName === "INPUT" || target.tagName === "TEXTAREA") return true;
  // `isContentEditable` is the spec-correct check (it accounts for
  // inheritance from an ancestor), but jsdom implements neither it nor the
  // `contentEditable` IDL setter — the attribute is checked directly too, so
  // this holds in a real browser and in a test that can only set the
  // attribute.
  return target.isContentEditable || target.getAttribute("contenteditable") === "true";
}

/** A control that already answers Enter (and Space) natively: a shortcut on
 *  Enter must leave it alone, or pressing the focused button would do both. */
export function isActivatableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target.closest("button, a[href], summary, [role='menuitem']") !== null;
}
