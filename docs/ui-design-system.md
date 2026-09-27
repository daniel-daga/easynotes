# EasyNotes UI design guide

This guide keeps new screens and components consistent with the existing
Flutter app. Use it when adding a feature, reviewing a visual change, or
deciding whether a UI pattern belongs in a shared component.

The product design lives in [design.md](./design.md). This guide describes how
that design is expressed in the interface.

## Visual foundation

- Build on Material 3 and the `ThemeData` in `app/lib/main.dart`. Use the
  current `ColorScheme`, text theme, and component themes instead of defining
  one-off colors, font sizes, corner radii, or elevations.
- Use the bundled Roboto font through the app theme. Use a named `textTheme`
  style for each text role. Adjust a theme style with `copyWith` only when a
  component needs a deliberate variation, such as error-colored destructive
  text.
- Prefer color scheme roles such as `surface`, `onSurfaceVariant`, `outline`,
  `error`, and their container variants. Keep note-color blending in
  `app/lib/ui/note_appearance.dart` so cards and the detail screen render a
  selected color the same way in light and dark themes.
- Use the spacing and shapes of the nearest Material component. If a new
  pattern needs a repeated custom value, give it one named source rather than
  scattering the value across screens.

## Product language

Use the same words throughout the app:

| Use | Meaning |
| --- | --- |
| **Note** | Any piece of saved content; the general term for the content being shared or edited. |
| **Checklist** | A note made of items with checkboxes. |
| **Label** | A user-created way to group notes. Keep “tag” for internal model names. |
| **Archive** / **Unarchive** | Move a note out of the main Notes view and bring it back. |
| **Trash** / **Restore** | Reversibly move a note out of its current view and return it. |
| **Delete permanently** | Remove a note with no undo. Reserve this wording for the irreversible action. |
| **Colour** | Use this spelling in user-facing copy. Code identifiers may follow existing API spelling. |
| **Your devices** | The account and recovery-phrase area. Do not call it sign-in or login. |

Prefer short, direct action labels: “Approve,” “Decline,” “Move to trash,” and
“Delete permanently.” Put any explanation in supporting text, not in a playful
or indirect action label. Use the same label for the same action wherever it
appears.

## Icons

- Use Material icons from `Icons`; follow the surrounding control's style.
  Outlined icons are the default for neutral actions. Use a filled variant to
  show an active state where the icon family provides one.
- Pick an icon for the action's meaning, not just its appearance. A pin action
  uses the pin icon in every context; archive, reminders, labels, sharing, and
  trash follow the same rule.
- When the same task appears in a toolbar, a sheet, or a filter, keep its icon
  consistent. If no suitable icon exists, prefer a clear text label to a
  misleading symbol.
- Give every icon-only control an action-oriented tooltip. Give selectable
  swatches, toggles, and other non-text controls a semantic label and selected
  state as well.

## Actions, menus, and dialogs

- Use `IconButton` for a small number of frequent actions that benefit from
  direct access. Use a bottom sheet for a group of related note actions so the
  same action set can be reached from a note card and its detail view.
- Keep note actions in the same order and use the same wording and icons in
  each entry point. Hide actions that do not apply to the note's state; for
  example, pinning and archiving do not apply while a note is in Trash.
- Keep personal placement actions (colour, pin, archive, trash) visually
  distinct from editing the note's shared content. Make the distinction clear
  in labels or placement when it affects what collaborators will see.
- Use a dialog when a person must make a considered choice or confirm an
  irreversible action. Say what will happen, identify consequences that matter,
  and offer a clear way to cancel.
- Dismiss a sheet before opening another route or dialog from its action. Keep
  the owning screen responsible for navigation and confirmation when those
  actions outlive the sheet.

## Text and hierarchy

- Use the app bar title for the screen's identity, a title text style for
  section headings, body styles for instructions and content, and label styles
  for compact supporting information. Avoid hard-coded font sizes and weights.
- Keep the same role at the same hierarchy level the same size and weight.
  Different screens can use different roles when their content has a different
  hierarchy.
- Use supporting text for explanations, counts, or status. Avoid making helper
  copy compete with the primary action or note content.
- Do not use a checklist-specific term for a plain note or call every note a
  list. Choose “note” for shared, generic actions and “checklist” for its
  checkbox-specific controls.

## Reusable components

Before adding a component:

1. Search `app/lib/ui/` for an existing component that already expresses the
   pattern. Extend it when the behavior and visual role match.
2. If the same interaction or visual unit is needed on more than one screen,
   extract it into a focused widget or helper under `app/lib/ui/`. Keep state
   and navigation with the screen that owns them; pass callbacks and display
   data into the reusable widget.
3. Keep presentation helpers free of storage, sync, or account decisions.
   Put shared note tinting and other display-only rules in the relevant
   appearance helper.
4. Give interactive widgets stable semantic labels and keys only where they
   support accessibility or a real UI contract. Do not make layout depend on
   test-only keys.

Prefer a small component with one visual responsibility over a general-purpose
framework. Do not extract a one-use widget just to shorten a build method; wait
until it has a second real use or a clear independent role.

## Layout and interaction

### Search

- Search uses a full-width header with a persistent “Back to Home” action.
  Leaving search clears its text and selected filter.
- Empty search shows the filter browser, including approved collaborators under People.
  Choose exactly one filter across Type, Labels, Reminders, People, and Colour, then
  optionally type to search within it. The field names the scope, for example
  “Search in Checklists.” Results have no filter chips or dropdowns.
- The always-visible × clears both text and the selected filter and returns
  to the filter browser. Manually deleting text keeps the selected filter.
- Search includes Notes and Archive, with archived notes marked. Search
  opened from Trash includes only Trash. Labels do not implicitly restrict
  search; select a label explicitly in the filter browser.

### General

- A note's person-plus action opens Add collaborators: owner, approved device
  identities, pending requests, invite links, and owner-only Remove everyone.
  Names are self-chosen; older shares may lack directory entries. People search
  matches device identity, so equal names do not merge different collaborators.
- Send a copy lives in the bottom-right overflow menu below Reminders. It opens
  the content preview with system sharing and copy actions.
- Add attachment (`add_box_outlined`) is the first control at the bottom left
  of the editor toolbar, before Colour and the checkbox switch. Its sheet
  offers Choose images and Audio file. Pictures show in a gallery above the
  title; sound files are rows below the content. An attachment's menu holds
  Save a copy, Describe and Remove attachment, in that order. Removal always
  confirms, because it changes the note for everyone on it. Transfer states
  ("Waiting to upload", "Couldn't upload" with Retry) sit on the attachment
  they concern, never on the whole note.

- Let content size itself with Material layout widgets. Use fixed dimensions
  only for controls whose target or visual geometry needs them, such as color
  swatches.
- Check narrow and wide layouts when adding a row, toolbar, or dialog. Long
  labels should wrap or scroll intentionally rather than overflow. Preserve
  comfortable touch targets and safe-area insets.
- Make state visible: selected colors, active filters, pin state, and checked
  items should not depend on color alone. Preserve keyboard focus and semantic
  behavior when composing custom controls.
- Keep ordinary sync status quiet. Show actionable failures where the person
  can find them without repeating status on every note.

## Adding a component: review checklist

- Does it use the theme's colors, text styles, shapes, and Material behavior?
- Does it use the established words and the same icon as the same action
  elsewhere?
- Is it using an existing shared component, or should this pattern now be
  shared?
- Are enabled, selected, empty, loading, error, and destructive states clear
  where relevant?
- Can a person reach and understand it with touch, keyboard, and assistive
  technology?
- Does it fit both narrow and wide screens, including long translated text?
- If it changes a design rule, have this guide and the product design been
  updated to agree?
