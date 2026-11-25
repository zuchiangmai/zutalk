# ZuTalk 0.8.6

City sign-in and archive filing now work with Pol V0.3 cities (such as
`zuchiangmai.com`).

ZuTalk requires macOS 12.5 or later.

## City sign-in and archive on Pol V0.3

- **QR sign-in updated for Pol V0.3.** When you sign in to a Pol V0.3 city in
  Settings › Account, ZuTalk generates a Zu relay sign-in code that your phone
  can scan with the system camera, Passport scanner, or City hall › Sign in
  another device — or paste directly as a link.
- **Six-digit confirmation on this Mac.** After you approve the request on your
  phone, both screens display the same six-digit check code. Compare the two
  screens and choose **Match — Sign In** on your Mac to verify your DID key and
  complete sign-in, or **Doesn’t Match** to discard the code and request a new
  one.
- **Calendar, channels, transcript, and audio filing.** Signed-in residents read
  the V0.3 city calendar and archive channels, file transcripts and translations
  as signed Zu records and blobs, upload `.m4a` audio parts, open the resulting
  archive page directly, and withdraw items from the archive. Cities still on
  the previous protocol remain supported automatically.

Verified with end-to-end Rust and Swift tests covering V0.3 QR key unsealing,
six-digit check matching, DID verification, calendar reduction, transcript and
audio filing, and archive withdrawal.
