//! Security layer 1: risky code on the lines one agent added.
use gitta_core::code_security::{findings, load, Note};
use gitta_core::Snapshot;

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }

/// The notes for `added` appended to `base` in `path`.
fn notes(path: &str, base: &str, added: &str) -> Vec<Note> {
    let (b, n) = (snap(&[(path, base)]), snap(&[(path, &format!("{base}{added}"))]));
    findings(&b, &n, &[path.to_string()])
}

fn rules(path: &str, added: &str) -> Vec<String> { notes(path, "x = 1\n", added).into_iter().map(|n| n.rule).collect() }

/// Each case: (path, risky lines, the rule, the safe form of the same thing).
const CASES: &[(&str, &str, &str, &str)] = &[
    // Python
    ("app/db.py", "    cur.execute(f\"SELECT * FROM users WHERE name = '{name}'\")\n", "sql-text", "    cur.execute(\"SELECT * FROM users WHERE name = %s\", (name,))\n"),
    ("app/db.py", "    q = \"SELECT * FROM t WHERE id = \" + uid\n    cur.execute(q)\n", "sql-text", "    q = \"SELECT * FROM t WHERE id = %s\"\n    cur.execute(q, (uid,))\n"),
    ("app/run.py", "    subprocess.run(f\"tar czf {name}.tgz {path}\", shell=True)\n", "shell-text", "    subprocess.run([\"tar\", \"czf\", name + \".tgz\", path])\n"),
    ("app/run.py", "    os.system(\"rm -rf \" + folder)\n", "shell-text", "    shutil.rmtree(folder)\n"),
    ("app/calc.py", "    return eval(expr)\n", "eval-text", "    return ast.literal_eval(expr)\n"),
    ("app/load.py", "    return pickle.loads(blob)\n", "unsafe-deserialise", "    return json.loads(blob)\n"),
    ("app/load.py", "    cfg = yaml.load(text)\n", "unsafe-deserialise", "    cfg = yaml.load(text, Loader=yaml.SafeLoader)\n"),
    ("app/http.py", "    r = requests.get(url, verify=False)\n", "tls-off", "    r = requests.get(url, verify=True)\n"),
    ("app/settings.py", "CORS_ALLOW_ALL_ORIGINS = True\nCORS_ALLOW_CREDENTIALS = True\n", "cors-credentials", "CORS_ALLOWED_ORIGINS = [\"https://shop.example\"]\nCORS_ALLOW_CREDENTIALS = True\n"),
    ("app/views.py", "@csrf_exempt\ndef hook(request):\n    pass\n", "csrf-off", "@csrf_protect\ndef hook(request):\n    pass\n"),
    ("app/users.py", "    digest = hashlib.md5(password.encode()).hexdigest()\n", "weak-password-hash", "    digest = bcrypt.hashpw(password.encode(), bcrypt.gensalt())\n"),
    ("app/users.py", "    etag = hashlib.md5(body).hexdigest()\n", "", ""),
    ("app/conf.py", "SERVICE_TOKEN = \"a9f3k2m7q1\"\n", "hardcoded-secret", "SERVICE_TOKEN = os.environ[\"SERVICE_TOKEN\"]\n"),
    ("app/login.py", "    logger.info(f\"login {user} with {password}\")\n", "secret-in-log", "    logger.info(f\"login {user}, password length {len(password)}\")\n"),
    ("app/login.py", "    print(\"password reset sent to\", email)\n", "", ""),
    // TypeScript / JavaScript
    ("src/db.ts", "  await db.query(`SELECT * FROM orders WHERE id = ${id}`);\n", "sql-text", "  await db.query(\"SELECT * FROM orders WHERE id = $1\", [id]);\n"),
    ("src/run.js", "const { exec } = require('child_process');\nexec(`git log ${branch}`);\n", "shell-text", "const { execFile } = require('child_process');\nexecFile('git', ['log', branch]);\n"),
    ("src/re.js", "const m = re.exec(text);\n", "", ""),
    ("src/calc.js", "  return eval(input);\n", "eval-text", "  return JSON.parse(input);\n"),
    ("src/http.ts", "const agent = new https.Agent({ rejectUnauthorized: false });\n", "tls-off", "const agent = new https.Agent({ rejectUnauthorized: true });\n"),
    ("src/app.ts", "app.use(cors({ origin: '*', credentials: true }));\n", "cors-credentials", "app.use(cors({ origin: ['https://shop.example'], credentials: true }));\n"),
    ("src/log.ts", "  console.log('token', accessToken);\n", "secret-in-log", "  console.log('token set', accessToken !== undefined);\n"),
    // Go
    ("db/q.go", "\trows, err := db.Query(\"SELECT * FROM t WHERE name = '\" + name + \"'\")\n", "sql-text", "\trows, err := db.Query(\"SELECT * FROM t WHERE name = ?\", name)\n"),
    ("db/q.go", "\tq := fmt.Sprintf(\"DELETE FROM t WHERE id = %s\", id)\n\tdb.Exec(q)\n", "sql-text", "\tdb.Exec(\"DELETE FROM t WHERE id = ?\", id)\n"),
    ("cmd/run.go", "\tout, _ := exec.Command(\"sh\", \"-c\", \"ls \"+dir).Output()\n", "shell-text", "\tout, _ := exec.Command(\"ls\", dir).Output()\n"),
    ("net/tls.go", "\tcfg := &tls.Config{InsecureSkipVerify: true}\n", "tls-off", "\tcfg := &tls.Config{MinVersion: tls.VersionTLS12}\n"),
    ("net/cors.go", "\tc := cors.New(cors.Options{AllowedOrigins: []string{\"*\"}, AllowCredentials: true})\n", "cors-credentials", "\tc := cors.New(cors.Options{AllowedOrigins: []string{\"https://shop.example\"}, AllowCredentials: true})\n"),
    ("net/cors.go", "\tr.Use(cors.New(cors.Config{AllowOrigins: []string{\"*\"}, AllowCredentials: true}))\n", "cors-credentials", "\tr.Use(cors.New(cors.Config{AllowOrigins: []string{\"https://shop.example\"}, AllowCredentials: true}))\n"),
    ("auth/h.go", "\tsum := md5.Sum([]byte(password))\n", "weak-password-hash", "\thash, err := bcrypt.GenerateFromPassword([]byte(password), bcrypt.DefaultCost)\n"),
    ("auth/log.go", "\tlog.Printf(\"user %s token %s\", user, apiToken)\n", "secret-in-log", "\tlog.Printf(\"user %s logged in\", user)\n"),
    // Java
    ("src/Repo.java", "        stmt.executeQuery(\"SELECT * FROM users WHERE name = '\" + name + \"'\");\n", "sql-text", "        PreparedStatement ps = conn.prepareStatement(\"SELECT * FROM users WHERE name = ?\");\n"),
    ("src/Run.java", "        Runtime.getRuntime().exec(\"convert \" + file);\n", "shell-text", "        new ProcessBuilder(\"convert\", file).start();\n"),
    ("src/Load.java", "        ObjectInputStream in = new ObjectInputStream(socket.getInputStream());\n", "unsafe-deserialise", "        Order o = mapper.readValue(socket.getInputStream(), Order.class);\n"),
    ("src/Tls.java", "    TrustManager[] trustAllCerts = new TrustManager[] { new X509TrustManager() {\n", "tls-off", "    TrustManager[] managers = factory.getTrustManagers();\n"),
    ("src/Sec.java", "        http.csrf().disable();\n", "csrf-off", "        http.csrf(Customizer.withDefaults());\n"),
    ("src/Pw.java", "        byte[] h = MessageDigest.getInstance(\"SHA-1\").digest(password.getBytes());\n", "weak-password-hash", "        String h = encoder.encode(password);\n"),
    ("src/Login.java", "        log.info(\"login {} {}\", user, password);\n", "secret-in-log", "        log.info(\"login {}\", user);\n"),
    // Kotlin
    ("src/Repo.kt", "    jdbc.queryForList(\"SELECT * FROM t WHERE id = $id\")\n", "sql-text", "    jdbc.queryForList(\"SELECT * FROM t WHERE id = ?\", id)\n"),
    ("src/Repo.kt", "    val rows = conn.createStatement().executeQuery(\"SELECT * FROM t WHERE id = $id\")\n", "sql-text", "    val rows = conn.prepareStatement(\"SELECT * FROM t WHERE id = ?\").apply { setString(1, id) }.executeQuery()\n"),
    ("src/Http.kt", "    builder.hostnameVerifier { _, _ -> true }\n", "tls-off", "    builder.certificatePinner(pinner)\n"),
    ("src/Sec.kt", "    http { csrf { disable() } }\n", "csrf-off", "    http { csrf { } }\n"),
    ("src/Log.kt", "    println(\"secret is $clientSecret\")\n", "secret-in-log", "    println(\"secret is set\")\n"),
    // C#
    ("src/Repo.cs", "        var cmd = new SqlCommand($\"SELECT * FROM Users WHERE Name = '{name}'\", conn);\n", "sql-text", "        var cmd = new SqlCommand(\"SELECT * FROM Users WHERE Name = @name\", conn);\n"),
    ("src/Run.cs", "        Process.Start(\"cmd.exe\", \"/c dir \" + folder);\n", "shell-text", "        Process.Start(new ProcessStartInfo(\"dir\") { ArgumentList = { folder } });\n"),
    ("src/Load.cs", "        var obj = new BinaryFormatter().Deserialize(stream);\n", "unsafe-deserialise", "        var obj = JsonSerializer.Deserialize<Order>(stream);\n"),
    ("src/Http.cs", "        ServicePointManager.ServerCertificateValidationCallback = (s, c, ch, e) => true;\n", "tls-off", "        ServicePointManager.SecurityProtocol = SecurityProtocolType.Tls12;\n"),
    ("src/Startup.cs", "        policy.AllowAnyOrigin().AllowCredentials();\n", "cors-credentials", "        policy.WithOrigins(\"https://shop.example\").AllowCredentials();\n"),
    ("src/Home.cs", "    [IgnoreAntiforgeryToken]\n", "csrf-off", "    [ValidateAntiForgeryToken]\n"),
    ("src/Log.cs", "        _logger.LogInformation(\"Key {Key}\", apiKey);\n", "secret-in-log", "        _logger.LogInformation(\"Key loaded\");\n"),
    // Rust
    ("src/db.rs", "    sqlx::query(&format!(\"SELECT * FROM t WHERE name = '{}'\", name)).fetch_all(&pool).await?;\n", "sql-text", "    sqlx::query(\"SELECT * FROM t WHERE name = $1\").bind(name).fetch_all(&pool).await?;\n"),
    ("src/run.rs", "    Command::new(\"sh\").arg(\"-c\").arg(format!(\"ls {}\", dir)).output()?;\n", "shell-text", "    Command::new(\"ls\").arg(dir).output()?;\n"),
    ("src/http.rs", "    let c = reqwest::Client::builder().danger_accept_invalid_certs(true).build()?;\n", "tls-off", "    let c = reqwest::Client::builder().build()?;\n"),
    ("src/log.rs", "    println!(\"token {token}\");\n", "secret-in-log", "    println!(\"token set: {}\", !token.is_empty());\n"),
];

#[test]
fn every_language_flags_the_risk_and_not_the_safe_form() {
    for (path, risky, rule, safe) in CASES {
        let got = rules(path, risky);
        if rule.is_empty() { assert!(got.is_empty(), "{path}: no note expected for {risky:?}, got {got:?}"); continue; }
        assert!(got.iter().any(|r| r == rule), "{path}: expected {rule} for {risky:?}, got {got:?}");
        let got = rules(path, safe);
        assert!(got.is_empty(), "{path}: the safe form {safe:?} gave {got:?}");
    }
}

#[test]
fn severity_line_and_words() {
    let n = notes("app/run.py", "import os\n", "\n\ndef clean(folder):\n    os.system(\"rm -rf \" + folder)\n");
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!((n[0].line, n[0].severity.as_str(), n[0].rule.as_str()), (5, "high", "shell-text"));
    assert!(n[0].text.contains("shell command") && n[0].safer.contains("list"));
    let n = notes("app/db.py", "", "cur.execute(f\"SELECT * FROM t WHERE id = {i}\")\n");
    assert_eq!(n[0].severity, "medium");
    // Test code never needs a person: high becomes medium there.
    let n = notes("tests/test_http.py", "", "r = requests.get(url, verify=False)\n");
    assert_eq!(n[0].severity, "medium");
}

#[test]
fn a_pattern_that_already_existed_is_not_reported() {
    let risky = "def f(cur, name):\n    cur.execute(f\"SELECT * FROM t WHERE n = '{name}'\")\n    return requests.get(u, verify=False)\n";
    // Unchanged lines, a move, a re-indent and an edit elsewhere in the file: nothing new.
    assert!(notes("app/db.py", risky, "\ndef g():\n    return 2\n").is_empty());
    let (b, n) = (snap(&[("app/db.py", risky)]), snap(&[("app/db.py", &format!("import x\n\nclass A:\n{}", risky.lines().map(|l| format!("    {l}\n")).collect::<String>()))]));
    assert!(findings(&b, &n, &["app/db.py".into()]).is_empty());
    // A second copy of the same line is new.
    assert_eq!(notes("app/db.py", risky, "    requests.post(u, verify=False)\n").len(), 1);
    // Files that are not code are left out.
    assert!(rules("README.md", "requests.get(url, verify=False)\n").is_empty());
    // Comments are not code.
    assert!(rules("app/x.py", "# requests.get(url, verify=False) is never ok\n").is_empty());
}

#[test]
fn allow_marker_and_allow_file() {
    let n = notes("app/http.py", "", "r = requests.get(url, verify=False)  # gitta: allow tls-off the test device has a self-signed certificate\n");
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].allowed.as_deref(), Some("the test device has a self-signed certificate"));
    assert!(n[0].allow_new, "a marker on an added line comes from this change");
    // A marker for another rule allows nothing.
    assert!(notes("app/http.py", "", "r = requests.get(url, verify=False)  # gitta: allow sql-text no\n")[0].allowed.is_none());
    // Committed entries in `.gitta-security` are trusted; entries the change adds are not.
    load("[allow]\ntls-off app/devices/* devices use self-signed certificates\n");
    let n = notes("app/devices/cam.py", "", "r = requests.get(url, verify=False)\n");
    assert_eq!((n[0].allowed.as_deref(), n[0].allow_new), (Some("devices use self-signed certificates"), false));
    assert!(notes("app/other.py", "", "r = requests.get(url, verify=False)\n")[0].allowed.is_none());
    load("");
    let (b, n) = (snap(&[("app/x.py", "")]), snap(&[("app/x.py", "r = requests.get(url, verify=False)\n"), (".gitta-security", "[allow]\ntls-off app/* trust me\n")]));
    let f = findings(&b, &n, &["app/x.py".into(), ".gitta-security".into()]);
    assert_eq!((f[0].allowed.as_deref(), f[0].allow_new), (Some("trust me"), true));
}

#[test]
fn the_key_survives_a_line_shift() {
    let a = notes("app/db.py", "", "cur.execute(f\"SELECT * FROM t WHERE id = {i}\")\n");
    let b = notes("app/db.py", "", "\n\n\ncur.execute(f\"SELECT * FROM t WHERE id = {i}\")\n");
    assert_eq!(a[0].key, b[0].key);
    assert_ne!(a[0].line, b[0].line);
}
