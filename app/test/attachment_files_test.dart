/// What the picker and the thumbnailer do before the core sees a file: an
/// image over the core's limits is not read or decoded first.
library;

import 'dart:typed_data';

import 'package:cross_file/cross_file.dart';
import 'package:easynotes/core/attachment_files.dart';
import 'package:file_picker/file_picker.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;

/// A picker that returns what the test set, instead of opening a dialog.
class FakePicker extends FilePickerPlatform {
  FakePicker(this.files);

  final List<PlatformFile> files;

  @override
  Future<List<PlatformFile>> pickFiles({
    String? dialogTitle,
    String? initialDirectory,
    FileType type = FileType.any,
    List<String>? allowedExtensions,
    Function(FilePickerStatus)? onFileLoading,
    int compressionQuality = 0,
    AndroidOptions androidOptions = const AndroidOptions(),
    DarwinOptions darwinOptions = const DarwinOptions(),
    WindowsOptions windowsOptions = const WindowsOptions(),
    LinuxOptions linuxOptions = const LinuxOptions(),
    WebOptions webOptions = const WebOptions(),
  }) async => files;
}

/// One picked file whose size is known up front, so a read can be counted.
final class FakeFile extends PlatformFile {
  FakeFile(this.name, this.size);

  @override
  final String name;

  final int size;
  int reads = 0;

  @override
  Uri get uri => Uri.file('/tmp/$name');

  @override
  XFile get xFile => XFile('/tmp/$name');

  @override
  int? lengthSync() => size;

  @override
  Future<int?> length() async => size;

  @override
  Future<Uint8List> readAsBytes() async {
    reads++;
    return Uint8List.fromList([1]);
  }

  @override
  Stream<Uint8List> readAsByteStream() => Stream.value(Uint8List.fromList([1]));
}

/// A real, uniform 1-bit BMP of 8000 x 5001 pixels: 40,008,000 pixels, just
/// over the core's limit, but only about 5 MB of pixels.
Uint8List oversizedPicture() => img.encodeBmp(
  img.Image(
    width: 8000,
    height: 5001,
    format: img.Format.uint1,
    numChannels: 1,
  ),
);

void main() {
  late FilePickerPlatform picker;

  setUp(() => picker = FilePickerPlatform.instance);
  tearDown(() => FilePickerPlatform.instance = picker);

  test('an image over the core size limit is never read', () async {
    final big = FakeFile('big.png', (20 << 20) + 1);
    final atLimit = FakeFile('at.png', 20 << 20);
    final small = FakeFile('small.png', 10);
    FilePickerPlatform.instance = FakePicker([big, atLimit, small]);

    final picked = await const DeviceAttachmentFiles().pickImages();

    expect(picked.map((file) => file.name), ['at.png', 'small.png']);
    expect(big.reads, 0);
  });

  test('an image over 40 megapixels gets no preview', () async {
    // 8000 x 5001 is just over the core's 40,000,000-pixel limit. It is a
    // real, decodable image, so without the guard this makes a preview.
    expect(await makeThumbnail(oversizedPicture()), isNull);
  });

  test('an ordinary picture still gets a preview', () async {
    final png = img.encodePng(img.Image(width: 4, height: 3));
    expect(await makeThumbnail(png), isNotNull);
  });

  test('rubbish gets no preview', () async {
    expect(await makeThumbnail(Uint8List.fromList([1, 2, 3])), isNull);
  });
}
