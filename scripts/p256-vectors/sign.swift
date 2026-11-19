// Signs the P-256 vector inputs with Apple CryptoKit, the way the iOS app's
// Secure Enclave key signs (P256.Signing over the raw preimage, SHA-256 by
// CryptoKit), then normalizes S to the low half as Ligus requires.
//
//   cargo test -p ligus-core --test p256_vectors p256_vector_inputs -- --ignored
//     (with LIGUS_P256_INPUTS=/tmp/p256_inputs.jsonl)
//   swift scripts/p256-vectors/sign.swift /tmp/p256_inputs.jsonl \
//     > crates/ligus-core/tests/golden/p256_cryptokit.jsonl
import CryptoKit
import Foundation

func unhex(_ s: String) -> Data {
    var d = Data(); var i = s.startIndex
    while i < s.endIndex { let j = s.index(i, offsetBy: 2); d.append(UInt8(s[i..<j], radix: 16)!); i = j }
    return d
}
func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }

// The P-256 group order n, big-endian.
let n: [UInt8] = [0xff,0xff,0xff,0xff,0x00,0x00,0x00,0x00,0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,
                  0xbc,0xe6,0xfa,0xad,0xa7,0x17,0x9e,0x84,0xf3,0xb9,0xca,0xc2,0xfc,0x63,0x25,0x51]
let halfN: [UInt8] = [0x7f,0xff,0xff,0xff,0x80,0x00,0x00,0x00,0x7f,0xff,0xff,0xff,0xff,0xff,0xff,0xff,
                      0xde,0x73,0x7d,0x56,0xd3,0x8b,0xcf,0x42,0x79,0xdc,0xe5,0x61,0x7e,0x31,0x92,0xa8]
func greater(_ a: [UInt8], _ b: [UInt8]) -> Bool {
    for i in 0..<32 { if a[i] != b[i] { return a[i] > b[i] } }
    return false
}
func lowS(_ raw: Data) -> Data {
    let r = Array(raw.prefix(32)); var s = Array(raw.suffix(32))
    if greater(s, halfN) {
        var out = [UInt8](repeating: 0, count: 32); var borrow = 0
        for i in stride(from: 31, through: 0, by: -1) {
            var d = Int(n[i]) - Int(s[i]) - borrow; borrow = 0
            if d < 0 { d += 256; borrow = 1 }
            out[i] = UInt8(d)
        }
        s = out
    }
    return Data(r + s)
}

let key = P256.Signing.PrivateKey()
let pk = key.publicKey.x963Representation  // 65 bytes, 0x04 || X || Y
let lines = try! String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8).split(separator: "\n")
for line in lines {
    let obj = try! JSONSerialization.jsonObject(with: Data(line.utf8)) as! [String: Any]
    let preimage = unhex(obj["preimage"] as! String)
    let sig = lowS(try! key.signature(for: preimage).rawRepresentation)
    precondition(key.publicKey.isValidSignature(try! P256.Signing.ECDSASignature(rawRepresentation: sig), for: preimage))
    // The envelope's exact wire bytes (never re-encoded: the preimage covers them).
    let envText = obj["envelopeText"] as! String
    print("{\"label\":\"\(obj["label"] as! String)\",\"pk\":\"\(hex(pk))\",\"preimage\":\"\(hex(preimage))\",\"sig\":\"\(hex(sig))\",\"envelope\":\(envText)}")
}
