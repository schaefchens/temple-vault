# To do: sharing with groups

Status: requests collected on 2026-11-15 from an app that shares content from its spaces with groups – communities – through a
server that must not be able to read any of it. What the app needs, not how Temple should do it: that is Temple's to decide.

Today such an app does the group's cryptography in JavaScript, so its group keys and plaintext pass through the JS layer, and
it has to keep media and caches out of the vault by itself.

## 1. Group keys that never reach JavaScript

A group is a key shared by its members, given to them inside an invitation. The app needs Temple to:

- **keep a group's key in a space** and hand the app only a handle to it;
- **seal and open records** with it, each bound to where it belongs – a record moved to another place, or another group, must not
  open;
- **derive from it** what a server may know: an id for each record that every member computes alike but the server can't read back,
  and a token that proves membership without revealing the key;
- **pad** what it seals, so sizes say little.

## 2. Sealing to someone

Members send things only the group's editors may open – a suggestion, a report. The app needs key pairs kept in a space, sealing to
a public key without naming the sender, and opening with the secret key. The same pairs, one per member, let a new group key reach
each remaining member when someone is removed.

## 3. Twelve words for a group

A group made with twelve words of its own, which all of its keys come from: so its owner gets it back on another device, or hands
it over by handing over the words. The app needs phrases made and checked for a purpose other than a space – the same word lists,
the same typo check – and keys derived from them, without opening or creating a space.

## 4. Large files, sealed natively

Photos and videos – megabytes – sealed and opened in native code, streamed, never through JavaScript as one buffer. Plaintext only
as a temporary file, gone after use: while a video plays, or while a photo is saved to the device's photo library.

## 5. Caches that don't grow the vault

Room never comes back to the file, so whatever a space only caches makes it grow for good: a group's previews, an outbox of
changes not yet sent, a sync cursor rewritten every few seconds. The app needs:

- **to know** how small, often rewritten values behave – whether rewriting one grows a space, and by how much over months;
- **a place for disposable data** outside the vault: encrypted under a key that lives only while the space is open, gone when it
  locks, and – like the vault – giving away neither the space nor what it cached.

## 6. Rotation

A group's records sealed again under a new key, in bulk, after a member is removed – and the new key handed to everyone who stays,
per item 2.

## 7. Authorship, maybe

Signed records, so that a member can't pass something off as an editor's, even with the server's help.

## 8. Reading objects from native code

Requested 2026-11-26. An app plays audio kept in a space – readings, recitations – through a media player that reads natively,
piece by piece, so the audio never passes through JavaScript and is never written out decrypted (on iOS an
`AVAssetResourceLoader`-based source on the player's own queue, on Android a Media3 `DataSource` on its loader thread). The app
needs native code – Swift, Kotlin – to read an object's length and any byte range of it from a space that JavaScript opened:
while that space is open, refused once it locks, without opening the space a second time (Temple refuses a second open of a
space today).

**Built** 2026-11-26: `space.nativeReader()` gives a handle, `TempleReader` in Swift and Kotlin reads with it – in
bindings/react-native; README, [From native code](../README.md#from-native-code).
