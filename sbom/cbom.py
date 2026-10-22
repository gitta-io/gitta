#!/usr/bin/env python3
"""Gitta CBOM: CycloneDX 1.6 cryptography bill of materials from sbom/crypto-inventory.json plus Cargo.lock.

  python3 sbom/cbom.py cbom  INVENTORY [--version V] [-o FILE]   write the CBOM (stdout by default)
  python3 sbom/cbom.py check INVENTORY                           fail when Cargo.lock and the inventory disagree

An inventory may "include" others (paths relative to it); assets with the same id are merged (usedIn added).
"check" fails when a crate that looks like cryptography is in a lock file but not in "crates", when a listed
crate is no longer in any lock file, or when a crate's major.minor version is not the "reviewed" one.
Standard library only.
"""
import json, re, sys, time, uuid, tomllib
from pathlib import Path

# Crates that provide or wrap cryptography. A crate matching this in Cargo.lock must be in the inventory.
WATCH = re.compile(r"^(ring|aws-lc-.*|openssl.*|native-tls|boring.*|rustls.*|.*webpki.*|getrandom|rand(_.*)?|"
                   r"subtle|zeroize|digest|crypto-.*|.*sha1.*|sha2|sha3|md-?5|blake[23].*|hmac|hkdf|pbkdf2|argon2|"
                   r"bcrypt|scrypt|ed25519.*|x25519.*|curve25519.*|p256|p384|p521|ecdsa|rsa|elliptic-curve|"
                   r"aes.*|chacha20.*|poly1305|ghash|cipher|aead|signature|spki|pkcs\d+|der|x509.*|"
                   r"jsonwebtoken|jwt.*|ring-compat|orion|sodiumoxide|libsodium.*|security-framework.*|schannel)$")


def load(path):
    path = Path(path).resolve()
    inv = json.loads(path.read_text())
    inv["_dir"] = path.parent
    return inv


def chain(inv):
    """The inventory and everything it includes, includes first."""
    out = []
    for inc in inv.get("include", []):
        out += chain(load(inv["_dir"] / inc))
    return out + [inv]


def lock_versions(inv):
    """{crate: version} from every lock file of the inventory chain."""
    v = {}
    for i in chain(inv):
        for lock in i.get("locks", []):
            p = i["_dir"].parent / lock
            if not p.exists():
                continue
            for pkg in tomllib.loads(p.read_text()).get("package", []):
                v.setdefault(pkg["name"], pkg["version"])
    return v


def merged(inv):
    crates, external, assets = {}, {}, {}
    for i in chain(inv):
        crates.update(i.get("crates", {}))
        external.update(i.get("external", {}))
        for a in i.get("assets", []):
            if a["id"] in assets:
                assets[a["id"]]["usedIn"] = assets[a["id"]].get("usedIn", []) + a.get("usedIn", [])
            else:
                assets[a["id"]] = dict(a)
    return crates, external, assets


def check(inv):
    crates, _, assets = merged(inv)
    v = lock_versions(inv)
    errs = []
    for name in sorted(v):
        if WATCH.match(name) and name not in crates:
            errs.append(f"{name} {v[name]} is in Cargo.lock but not in the crypto inventory: add it to \"crates\" and its algorithms to \"assets\"")
    for name, c in sorted(crates.items()):
        if name not in v:
            errs.append(f"{name} is in the crypto inventory but in no Cargo.lock: remove it (and assets it alone provides)")
        elif ".".join(v[name].split(".")[:2]) != c["reviewed"]:
            errs.append(f"{name} is {v[name]} in Cargo.lock but the inventory was reviewed for {c['reviewed']}: re-check its algorithms and defaults, then update \"reviewed\"")
    known = set(crates) | set(merged(inv)[1])
    for a in assets.values():
        if a.get("provider") not in known:
            errs.append(f"asset {a['id']}: provider {a.get('provider')!r} is not a listed crate or external tool")
        for r in a.get("protocol", {}).get("cryptoRefs", []):
            if r not in assets:
                errs.append(f"asset {a['id']}: cryptoRef {r} is not an asset")
    for e in errs:
        print("crypto inventory:", e, file=sys.stderr)
    if not errs:
        print(f"crypto inventory OK: {len(crates)} crates, {len(assets)} assets")
    return 1 if errs else 0


ALG_KEYS = {"primitive", "parameterSetIdentifier", "curve", "executionEnvironment", "implementationPlatform",
            "certificationLevel", "mode", "padding", "cryptoFunctions", "classicalSecurityLevel", "nistQuantumSecurityLevel"}
CERT_KEYS = {"subjectName", "issuerName", "notValidBefore", "notValidAfter", "signatureAlgorithmRef",
             "subjectPublicKeyRef", "certificateFormat", "certificateExtension"}
MAT_KEYS = {"type", "id", "state", "creationDate", "activationDate", "updateDate", "expirationDate", "value", "format"}
PADDINGS = {"pkcs5", "pkcs7", "pkcs1v15", "oaep", "raw", "other", "unknown"}


def ref(aid):
    return f"crypto:{aid}"


def asset_component(a, assets):
    props, cp = [], {"assetType": a["assetType"]}
    if "oid" in a:
        cp["oid"] = a["oid"]
    if "algorithm" in a:
        alg = {}
        for k, val in a["algorithm"].items():
            if k == "padding" and val not in PADDINGS:
                props.append(("gitta:padding", val)); alg[k] = "other"
            elif k in ALG_KEYS:
                alg[k] = val
            else:
                props.append((f"gitta:{k}", str(val)))
        alg.setdefault("executionEnvironment", "software-plain-ram")
        alg.setdefault("implementationPlatform", "generic")
        alg.setdefault("certificationLevel", ["none"])
        cp["algorithmProperties"] = alg
    if "protocol" in a:
        p = a["protocol"]
        pp = {"type": p["type"], "version": p["version"]}
        suites = p.get("cipherSuites")
        if isinstance(suites, str):
            suites = next((x["protocol"]["cipherSuites"] for x in assets.values()
                           if x.get("id") == suites.removeprefix("same as ")), [])
        if suites:
            pp["cipherSuites"] = [{"name": s} for s in suites]
        if p.get("cryptoRefs"):
            pp["cryptoRefArray"] = [ref(r) for r in p["cryptoRefs"]]
        for k in ("keyExchangeGroups",):
            if k in p:
                props.append((f"gitta:tls:{k}", ", ".join(p[k])))
        cp["protocolProperties"] = pp
    if "certificate" in a:
        c = a["certificate"]
        cp["certificateProperties"] = {k: v for k, v in c.items() if k in CERT_KEYS}
        props += [(f"gitta:{k}", v) for k, v in c.items() if k not in CERT_KEYS]
    if "material" in a:
        m = a["material"]
        mp = {k: v for k, v in m.items() if k in MAT_KEYS}
        if "algorithmRef" in m:
            mp["algorithmRef"] = ref(m["algorithmRef"])
        bits = re.fullmatch(r"(\d+) bits", str(m.get("size", "")))
        if bits:
            mp["size"] = int(bits.group(1))
        elif "size" in m:
            props.append(("gitta:size", m["size"]))
        cp["relatedCryptoMaterialProperties"] = mp
    for u in a.get("usedIn", []):
        props.append(("gitta:usedIn", f"{u['component']}: {u['purpose']}"))
    comp = {"type": "cryptographic-asset", "bom-ref": ref(a["id"]), "name": a["name"], "cryptoProperties": cp}
    locs = sorted({l.strip() for u in a.get("usedIn", []) for l in u.get("location", "").split(",") if l.strip()})
    if locs:
        comp["evidence"] = {"occurrences": [{"location": l} for l in locs]}
    if props:
        comp["properties"] = [{"name": k, "value": v} for k, v in props]
    return comp


def cbom(inv, version):
    crates, external, assets = merged(inv)
    v = lock_versions(inv)
    prod = inv["product"]
    prod_ref = f"pkg:cargo/{prod['name']}@{version}"
    libs, deps = [], {}
    for name, c in sorted(crates.items()):
        if name not in v:
            continue
        r = f"pkg:cargo/{name}@{v[name]}"
        libs.append({"type": "library", "bom-ref": r, "name": name, "version": v[name], "purl": r,
                     "description": c["role"]})
        deps[name] = r
    for name, desc in sorted(external.items()):
        r = f"external:{name}"
        libs.append({"type": "application", "bom-ref": r, "name": name, "scope": "excluded", "description": desc})
        deps[name] = r
    comps = libs + [asset_component(a, assets) for a in assets.values()]
    provides = {}
    for a in assets.values():
        provides.setdefault(deps[a["provider"]], []).append(ref(a["id"]))
    epoch = int(__import__("os").environ.get("SOURCE_DATE_EPOCH", time.time()))
    return {
        "$schema": "http://cyclonedx.org/schema/bom-1.6.schema.json",
        "bomFormat": "CycloneDX", "specVersion": "1.6",
        "serialNumber": f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, f'gitta-cbom:{prod_ref}')}",
        "version": 1,
        "metadata": {
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch)),
            "lifecycles": [{"phase": "build"}],
            "tools": {"components": [{"type": "application", "name": "gitta sbom/cbom.py"}]},
            "component": {"type": "application", "bom-ref": prod_ref, "name": prod["name"], "version": version,
                          "description": prod["description"], "purl": prod_ref},
            "properties": [{"name": "gitta:cbom:source", "value": "sbom/crypto-inventory.json + Cargo.lock"}],
        },
        "components": comps,
        "dependencies": [{"ref": prod_ref, "dependsOn": sorted(deps.values())}]
                        + [{"ref": r, "provides": sorted(p)} for r, p in sorted(provides.items())],
    }


def main(argv):
    if len(argv) < 2 or argv[0] not in ("cbom", "check"):
        print(__doc__, file=sys.stderr); return 2
    inv = load(argv[1])
    if argv[0] == "check":
        return check(inv)
    opt = lambda f, d=None: argv[argv.index(f) + 1] if f in argv else d
    version = opt("--version") or lock_versions(inv).get(inv["product"]["name"], "0.0.0")
    text = json.dumps(cbom(inv, version), indent=2) + "\n"
    (Path(opt("-o")).write_text(text) if opt("-o") else sys.stdout.write(text))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
