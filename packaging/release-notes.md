# ZuTalk 0.8.8

Tells you when a city has moved to a new system and your sign-in needs
renewing, instead of quietly showing an old list of events, and gives you a way
round when the update server cannot be reached.

ZuTalk requires macOS 12.5 or later.

## Cities

- **A city that changed systems now says so.** A sign-in made before a city
  moved to its new system stops working after the move. ZuTalk used to answer
  “try again in a little while” and keep showing the events it had read before
  the move. It now says the sign-in no longer works: the event picker shows
  **Sign In Again**, and Settings › Account says what to do.
- **If your list of events stopped updating:** open Settings › Account, sign
  out, then sign in again. The list fills in as soon as you are signed in.

## Updates

- **A check that cannot reach the update server offers the download page.**
  When you choose Check for Updates… and the server cannot be reached, ZuTalk
  tries once more by itself. If that fails too, it says so and offers
  **Try Again** and **Open Download Page**, so the new version can be fetched
  in your browser.
