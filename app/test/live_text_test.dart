import 'package:easynotes/ui/live_text.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('a splice counts characters, not UTF-16 units', () {
    final splice = bodySplice('café 😀', 'café 😀!');
    expect(splice.at, 6);
    expect(splice.removed, 0);
    expect(splice.inserted, '!');
  });

  test('a splice kept inside a surrogate pair leaves the pair whole', () {
    final splice = bodySplice('a😀b', 'a😀c');
    expect(splice.at, 2);
    expect(splice.removed, 1);
    expect(splice.inserted, 'c');
  });
}
