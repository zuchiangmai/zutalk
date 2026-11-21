// Makes webcrypto.json: one login between devices (login.md), computed with the WebCrypto API
// that login.md names, and with the DRISL bytes written out by hand. It shares no code with
// the kernel, so the kernel agreeing with it byte for byte says the two read the
// specification the same way. Run: node webcrypto.mjs > webcrypto.json
import { createECDH, webcrypto } from 'node:crypto'

const { subtle } = webcrypto
const hex = (bytes) => Buffer.from(bytes).toString('hex')
const text = (string) => new TextEncoder().encode(string)
const cat = (...parts) => Buffer.concat(parts.map((part) => Buffer.from(part)))
const b64 = (bytes) => Buffer.from(bytes).toString('base64url')

// What the two sides draw at random, fixed here.
const receiverSecret = Buffer.alloc(32, 0x11)
const room = Buffer.from([...Array(16).keys()])
const senderSecret = Buffer.alloc(32, 0x22)
const iv = Buffer.alloc(12, 0x33)
// What is handed over: the private Zu signing key of the DID that the specifications use.
const did = 'did:zu:ewvi7nxzyoun6zhxrhs64oiz'
const seed = Buffer.alloc(32, 0x07)
const origin = 'https://pol.4seas.example'

// A P-256 key pair from its private scalar: the public key as 65 bytes, and both as keys.
async function pair(secret) {
  const ecdh = createECDH('prime256v1')
  ecdh.setPrivateKey(secret)
  const point = ecdh.getPublicKey()
  const jwk = { kty: 'EC', crv: 'P-256', x: b64(point.subarray(1, 33)), y: b64(point.subarray(33)) }
  const algorithm = { name: 'ECDH', namedCurve: 'P-256' }
  const privateKey = await subtle.importKey('jwk', { ...jwk, d: b64(secret) }, algorithm, false, ['deriveBits'])
  const publicKey = await subtle.importKey('raw', point, algorithm, false, [])
  return { point, privateKey, publicKey }
}

// § The Code.
const receiver = await pair(receiverSecret)
const roomUrl = `${origin}/.well-known/zu-relay/${hex(room)}`
const code = `${roomUrl}#${b64(receiver.point)}`

// § Messages: the key message. In DRISL the names come shortest first: v, did, key, $type.
const keyMessage = cat(
  [0xa4],
  [0x61], text('v'), [0x01],
  [0x63], text('did'), [0x78, did.length], text(did),
  [0x63], text('key'), [0x58, 32], seed,
  [0x65], text('$type'), [0x63], text('key'),
)

// § Sealing the Key.
const sender = await pair(senderSecret)
const shared = await subtle.deriveBits({ name: 'ECDH', public: receiver.publicKey }, sender.privateKey, 256)
const hkdf = await subtle.importKey('raw', shared, 'HKDF', false, ['deriveKey'])
const salt = cat(receiver.point, sender.point)
const key = await subtle.deriveKey(
  { name: 'HKDF', hash: 'SHA-256', salt, info: text('zu-login-1') },
  hkdf,
  { name: 'AES-GCM', length: 256 },
  false,
  ['encrypt'],
)
const ct = Buffer.from(
  await subtle.encrypt({ name: 'AES-GCM', iv, additionalData: text(roomUrl), tagLength: 128 }, key, keyMessage),
)

// The sealed message. The names come shortest first: v, ct, iv, epk, $type.
const sealed = cat(
  [0xa5],
  [0x61], text('v'), [0x01],
  [0x62], text('ct'), [0x58, ct.length], ct,
  [0x62], text('iv'), [0x4c], iv,
  [0x63], text('epk'), [0x58, 65], sender.point,
  [0x65], text('$type'), [0x66], text('sealed'),
)

const vector = {
  origin,
  receiverSecret: hex(receiverSecret),
  room: hex(room),
  code,
  did,
  seed: hex(seed),
  keyMessage: hex(keyMessage),
  senderSecret: hex(senderSecret),
  iv: hex(iv),
  sealed: hex(sealed),
}
console.log(JSON.stringify(vector, null, 2))
