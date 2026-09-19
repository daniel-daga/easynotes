/// What [whenPageHidden] is outside a browser: nothing.
///
/// Here for the reason `indexed_db_stub.dart` is — to keep `dart:js_interop`
/// out of a build with no JavaScript in it. A device with a disk never has a
/// page to hide, and its store has written the edit before `append` returned.
library;

void Function() whenPageHidden(void Function() onHidden) => () {};
