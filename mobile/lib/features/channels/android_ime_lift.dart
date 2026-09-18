import 'dart:math' show max;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

/// Android keeps the message viewport fixed while the IME animates. Only this
/// small wrapper follows the frame-by-frame inset; timelines apply the final
/// inset once metrics settle.
class AndroidImeLift extends StatelessWidget {
  /// The composer subtree that follows the animated Android IME inset.
  final Widget child;

  /// Whether the keyboard behind the reported inset belongs to [child].
  ///
  /// Android can keep reporting the previous route's IME inset after the
  /// keyboard is visually gone, and a route pushed while another route's
  /// composer had focus sees that inset at its first build. A composer that
  /// does not own the keyboard must not lift for it, or it floats mid-screen
  /// over a blank band where nothing is. Defaults to `true`, which is the
  /// right answer for a route whose own composer put the keyboard up.
  final bool ownsKeyboard;

  /// Creates a wrapper that lifts [child] without resizing its surrounding
  /// message viewport on Android.
  const AndroidImeLift({
    super.key,
    required this.child,
    this.ownsKeyboard = true,
  });

  @override
  Widget build(BuildContext context) {
    if (!usesFixedAndroidImeViewport) return child;
    final imeBottom = MediaQuery.viewInsetsOf(context).bottom;
    final systemBottom = MediaQuery.viewPaddingOf(context).bottom;
    // Keep the tree shape identical whether or not the keyboard is owned:
    // toggling ownership must only change the padding value. Swapping the
    // Padding in and out would rebuild the composer subtree from scratch and
    // drop its expanded state the moment its field took focus.
    final lift = ownsKeyboard ? max(0.0, imeBottom - systemBottom) : 0.0;
    return Padding(
      // The composer already reserves [systemBottom]. Android's IME inset
      // includes that navigation area, so lifting by the full value leaves a
      // second safe-area gap above the keyboard.
      padding: EdgeInsets.only(bottom: lift),
      child: child,
    );
  }
}

/// Whether channel and thread scaffolds should keep a fixed viewport while
/// Android IME insets animate, with their composer lifted independently.
bool get usesFixedAndroidImeViewport =>
    defaultTargetPlatform == TargetPlatform.android;
