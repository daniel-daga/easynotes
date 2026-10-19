import 'package:flutter/foundation.dart';

/// App visibility shared with pollers that outlive individual screens.
final ValueNotifier<bool> appIsForeground = ValueNotifier<bool>(true);
