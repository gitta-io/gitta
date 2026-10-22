//! Security layer 2: a weakened security control that another agent's new code relies on.
use gitta_core::guard::safety_net;
use gitta_core::{analyze, Snapshot, Warning};

fn snap(files: &[(&str, &str)]) -> Snapshot { files.iter().map(|(p, t)| (p.to_string(), t.as_bytes().to_vec())).collect() }

fn security(w: &[Warning]) -> Vec<&Warning> {
    w.iter().filter(|x| x.kind == "SECURITY" && x.severity.as_deref() == Some("high") && x.referenced_by.as_deref() == Some("b")).collect()
}

fn own_findings(base: &Snapshot, new: &Snapshot) -> Vec<String> {
    let paths: Vec<String> = base.keys().chain(new.keys()).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    safety_net(base, new, &paths).into_iter().filter(|f| f.kind == "SECURITY_WEAKENED").map(|f| f.detail).collect()
}

/// Agent a weakens the control in `lib` (or makes a harmless edit); agent b adds `caller`, which uses it.
struct Case { lib: &'static str, base: &'static str, weakened: String, harmless: String, caller_path: &'static str, caller: &'static str, want: &'static str }

fn run(c: Case) {
    let base = snap(&[(c.lib, c.base)]);
    let mut b = base.clone(); b.insert(c.caller_path.into(), c.caller.as_bytes().to_vec());
    let mut a = base.clone(); a.insert(c.lib.into(), c.weakened.clone().into_bytes());
    let w = analyze(&base, ("a", &a), ("b", &b));
    let s = security(&w);
    assert!(!s.is_empty(), "expected SECURITY, got {w:?}");
    assert!(s.iter().all(|x| x.path == c.caller_path && x.changed_by.as_deref() == Some("a")));
    let note = s[0].security.as_ref().expect("note");
    assert!(note.what.contains(c.want), "{note:?}");
    // The agent's own weakening needs a person's approval before it lands.
    assert!(!own_findings(&base, &a).is_empty(), "expected SECURITY_WEAKENED for the weakened file");
    // A harmless edit, or a stricter check, is not a security hazard and needs no approval.
    let mut a2 = base.clone(); a2.insert(c.lib.into(), c.harmless.clone().into_bytes());
    let w2 = analyze(&base, ("a", &a2), ("b", &b));
    assert!(security(&w2).is_empty(), "unexpected SECURITY: {w2:?}");
    assert!(own_findings(&base, &a2).is_empty(), "unexpected finding: {:?}", own_findings(&base, &a2));
}

const PY_VIEWS: &str = "from django.contrib.auth.decorators import login_required\nfrom django.core.exceptions import PermissionDenied\n\n\n@login_required\ndef export_orders(request):\n    return list_orders(request)\n\n\ndef list_orders(request):\n    return []\n";

#[test]
fn python_login_decorator_removed() {
    run(Case { lib: "shop/views.py", base: PY_VIEWS, want: "removed the login check (`@login_required`) from",
        weakened: PY_VIEWS.replace("@login_required\ndef export", "def export"),
        harmless: PY_VIEWS.replace("    return list_orders(request)\n", "    if not request.user.is_staff:\n        raise PermissionDenied\n    return list_orders(request)\n"),
        caller_path: "shop/reports.py", caller: "from shop.views import export_orders\n\n\ndef report(request):\n    return export_orders(request)\n" });
    // Commenting the decorator out removes it too.
    let base = snap(&[("shop/views.py", PY_VIEWS)]);
    let new = snap(&[("shop/views.py", &PY_VIEWS.replace("@login_required\ndef", "# @login_required\ndef"))]);
    assert!(!own_findings(&base, &new).is_empty());
    // Django REST framework: the permission class of a view set loosened to AllowAny.
    let v = "from rest_framework import viewsets\nfrom rest_framework.permissions import IsAuthenticated\n\n\nclass OrderViewSet(viewsets.ModelViewSet):\n    permission_classes = [IsAuthenticated]\n\n    def list(self, request):\n        return []\n";
    let base = snap(&[("api/views.py", v)]);
    let mut a = base.clone(); a.insert("api/views.py".into(), v.replace("[IsAuthenticated]", "[AllowAny]").into_bytes());
    let mut b = base.clone(); b.insert("api/urls.py".into(), b"from api.views import OrderViewSet\n\n\ndef routes(router):\n    router.register('orders', OrderViewSet)\n".to_vec());
    let w = analyze(&base, ("a", &a), ("b", &b));
    assert!(security(&w).iter().any(|x| x.symbol.as_deref() == Some("api.views.OrderViewSet")), "{w:?}");
}

#[test]
fn python_check_function_bypassed_and_strict_default() {
    let auth = "def check_permission(user, perm):\n    if perm not in user.perms:\n        raise PermissionError(perm)\n    return True\n\n\ndef fetch(url, verify=True):\n    return get(url, verify=verify)\n";
    let caller = "from app.auth import check_permission\n\n\ndef delete(user, item):\n    check_permission(user, 'delete')\n    item.delete()\n";
    run(Case { lib: "app/auth.py", base: auth, want: "removed a refusal",
        weakened: auth.replace("    if perm not in user.perms:\n        raise PermissionError(perm)\n", ""),
        harmless: auth.replace("    return True\n", "    log(user, perm)\n    return True\n"),
        caller_path: "app/items.py", caller });
    run(Case { lib: "app/auth.py", base: auth, want: "let callers through early",
        weakened: auth.replace("    if perm not in", "    return True\n    if perm not in"),
        harmless: auth.replace("    if perm not in user.perms:", "    if not user.active or perm not in user.perms:"),
        caller_path: "app/items.py", caller });
    let fetcher = "from app.auth import fetch\n\n\ndef load():\n    return fetch('https://x')\n";
    run(Case { lib: "app/auth.py", base: auth, want: "made `verify` default to off",
        weakened: auth.replace("verify=True", "verify=False"), harmless: auth.replace("return get(url", "return  get(url"),
        caller_path: "app/load.py", caller: fetcher });
}

#[test]
fn python_config_marked_symbols_and_paths() {
    // Not named like a check: only the repository's mark makes it one.
    let billing = "def apply_limits(account):\n    if account.used > account.quota:\n        raise OverQuota()\n    return account\n";
    let caller = "from billing import apply_limits\n\n\ndef charge(account):\n    apply_limits(account)\n";
    let base = snap(&[("billing.py", billing)]);
    let mut a = base.clone(); a.insert("billing.py".into(), billing.replace("    if account.used > account.quota:\n        raise OverQuota()\n", "").into_bytes());
    let mut b = base.clone(); b.insert("pay.py".into(), caller.as_bytes().to_vec());
    assert!(security(&analyze(&base, ("a", &a), ("b", &b))).is_empty(), "unmarked symbol must not be a security hazard");
    gitta_core::security::set_marked(vec!["billing.apply_limits".into()]);
    let w = analyze(&base, ("a", &a), ("b", &b));
    gitta_core::security::set_marked(vec!["secure/*".into()]);
    let base2 = snap(&[("secure/quota.py", billing)]);
    let mut a2 = base2.clone(); a2.insert("secure/quota.py".into(), billing.replace("        raise OverQuota()\n", "        pass\n").into_bytes());
    let mut b2 = base2.clone(); b2.insert("pay.py".into(), caller.replace("from billing", "from secure.quota").into_bytes());
    let w2 = analyze(&base2, ("a", &a2), ("b", &b2));
    gitta_core::security::set_marked(vec![]);
    assert!(!security(&w).is_empty(), "marked symbol: {w:?}");
    assert!(!security(&w2).is_empty(), "marked path: {w2:?}");
}

const TS_CTRL: &str = "import { Controller, Get, UseGuards } from '@nestjs/common';\nimport { AuthGuard } from '@nestjs/passport';\n\n@Controller('orders')\nexport class OrdersController {\n  @UseGuards(AuthGuard('jwt'))\n  @Get('export')\n  exportOrders() {\n    return [];\n  }\n\n  @Get('count')\n  count() {\n    return 0;\n  }\n}\n";

#[test]
fn typescript_guard_removed() {
    run(Case { lib: "src/orders.controller.ts", base: TS_CTRL, want: "removed the guard (`@UseGuards`) from",
        weakened: TS_CTRL.replace("  @UseGuards(AuthGuard('jwt'))\n", ""),
        harmless: TS_CTRL.replace("    return 0;", "    return 1;"),
        caller_path: "src/report.ts", caller: "import { OrdersController } from './orders.controller';\n\nexport function report(c: OrdersController) {\n  return c.exportOrders();\n}\n" });
    // Express: the login middleware taken off a route handler chain.
    let r = "import { requireAuth } from './auth';\n\nexport function exportRoute(app) {\n  app.get('/orders/export', requireAuth, exportOrders);\n}\n";
    run(Case { lib: "src/routes.ts", base: r, want: "removed the login middleware",
        weakened: r.replace("requireAuth, exportOrders", "exportOrders"), harmless: r.replace("'/orders/export'", "'/orders/export.csv'"),
        caller_path: "src/server.ts", caller: "import { exportRoute } from './routes';\n\nexport function start(app) {\n  exportRoute(app);\n}\n" });
}

const GO_AUTH: &str = "package auth\n\nfunc CheckPermission(u *User, p string) error {\n\tif !u.Has(p) {\n\t\treturn ErrForbidden\n\t}\n\treturn nil\n}\n";

#[test]
fn go_check_bypassed() {
    run(Case { lib: "internal/auth/auth.go", base: GO_AUTH, want: "let callers through early",
        weakened: GO_AUTH.replace("\tif !u.Has(p) {", "\treturn nil\n\tif !u.Has(p) {"),
        harmless: GO_AUTH.replace("\tif !u.Has(p) {", "\tif u == nil || !u.Has(p) {"),
        caller_path: "internal/report/report.go", caller: "package report\n\nimport \"example.com/app/internal/auth\"\n\nfunc Run(u *auth.User) error {\n\treturn auth.CheckPermission(u, \"export\")\n}\n" });
    // Middleware taken off a route.
    let r = "package web\n\nfunc Routes(r Router) {\n\tr.Handle(\"/orders/export\", RequireAuth(exportHandler))\n}\n";
    let base = snap(&[("web/routes.go", r)]);
    assert!(!own_findings(&base, &snap(&[("web/routes.go", &r.replace("RequireAuth(exportHandler)", "exportHandler"))])).is_empty());
}

const RS_AUTH: &str = "pub fn check_permission(user: &User, perm: &str) -> Result<(), Denied> {\n    if !user.has(perm) {\n        return Err(Denied);\n    }\n    Ok(())\n}\n";

#[test]
fn rust_check_bypassed() {
    run(Case { lib: "src/auth.rs", base: RS_AUTH, want: "removed a refusal",
        weakened: RS_AUTH.replace("    if !user.has(perm) {\n        return Err(Denied);\n    }\n", ""),
        harmless: RS_AUTH.replace("    if !user.has(perm) {", "    if !user.active || !user.has(perm) {"),
        caller_path: "src/report.rs", caller: "use crate::auth::check_permission;\n\npub fn report(u: &User) {\n    check_permission(u, \"export\").unwrap();\n}\n" });
}

const JAVA_CTRL: &str = "package com.acme.web;\n\nimport org.springframework.security.access.prepost.PreAuthorize;\n\npublic class OrderController {\n    @PreAuthorize(\"hasRole('ADMIN')\")\n    public List<Order> exportOrders() {\n        return orders.all();\n    }\n\n    public int count() {\n        return 0;\n    }\n}\n";

#[test]
fn java_annotation_removed() {
    run(Case { lib: "src/main/java/com/acme/web/OrderController.java", base: JAVA_CTRL, want: "removed the permission check (`@PreAuthorize`) from",
        weakened: JAVA_CTRL.replace("    @PreAuthorize(\"hasRole('ADMIN')\")\n", ""),
        harmless: JAVA_CTRL.replace("hasRole('ADMIN')", "hasRole('ADMIN') and isAuthenticated()").replace("return 0;", "return 1;"),
        caller_path: "src/main/java/com/acme/web/Report.java",
        caller: "package com.acme.web;\n\npublic class Report {\n    public int size(OrderController c) {\n        return c.exportOrders().size();\n    }\n}\n" });
}

const CS_CTRL: &str = "using Microsoft.AspNetCore.Authorization;\n\nnamespace Shop.Web;\n\npublic class OrdersController : Controller\n{\n    [Authorize(Roles = \"Admin\")]\n    [ValidateAntiForgeryToken]\n    public IActionResult Export()\n    {\n        return Ok();\n    }\n\n    public int Count() => 0;\n}\n";

#[test]
fn csharp_attribute_removed() {
    run(Case { lib: "src/Shop.Web/OrdersController.cs", base: CS_CTRL, want: "removed the login check (`[Authorize`) from",
        weakened: CS_CTRL.replace("    [Authorize(Roles = \"Admin\")]\n", ""),
        harmless: CS_CTRL.replace("Count() => 0", "Count() => 1"),
        caller_path: "src/Shop.Web/Report.cs",
        caller: "namespace Shop.Web;\n\npublic class Report\n{\n    public object Run(OrdersController c) => c.Export();\n}\n" });
    // [AllowAnonymous] added: an opener.
    let base = snap(&[("src/Shop.Web/OrdersController.cs", CS_CTRL)]);
    let new = snap(&[("src/Shop.Web/OrdersController.cs", &CS_CTRL.replace("    [ValidateAntiForgeryToken]\n", "    [ValidateAntiForgeryToken]\n    [AllowAnonymous]\n"))]);
    assert!(own_findings(&base, &new).iter().any(|d| d.contains("allowed anonymous access")), "{:?}", own_findings(&base, &new));
}

const KT_CTRL: &str = "package shop.web\n\nimport org.springframework.security.access.prepost.PreAuthorize\n\nclass OrderController {\n    @PreAuthorize(\"hasRole('ADMIN')\")\n    fun exportOrders(): List<Order> {\n        return orders.all()\n    }\n\n    fun count(): Int = 0\n}\n";

#[test]
fn kotlin_annotation_removed() {
    run(Case { lib: "src/main/kotlin/shop/web/OrderController.kt", base: KT_CTRL, want: "removed the permission check (`@PreAuthorize`) from",
        weakened: KT_CTRL.replace("    @PreAuthorize(\"hasRole('ADMIN')\")\n", ""),
        harmless: KT_CTRL.replace("Int = 0", "Int = 1"),
        caller_path: "src/main/kotlin/shop/web/Report.kt",
        caller: "package shop.web\n\nfun size(c: OrderController): Int {\n    return c.exportOrders().size\n}\n" });
}

#[test]
fn unrelated_edits_to_an_auth_module_are_quiet() {
    // A new helper next to the checks, and a new stricter decorator: nothing weakened.
    let base = snap(&[("shop/views.py", PY_VIEWS)]);
    let new = PY_VIEWS.replace("def list_orders", "@login_required\ndef list_orders") + "\n\ndef helper():\n    return 1\n";
    let mut a = base.clone(); a.insert("shop/views.py".into(), new.clone().into_bytes());
    let mut b = base.clone(); b.insert("shop/reports.py".into(), b"from shop.views import export_orders, list_orders\n\n\ndef report(r):\n    return export_orders(r), list_orders(r)\n".to_vec());
    let w = analyze(&base, ("a", &a), ("b", &b));
    assert!(w.iter().all(|x| x.kind != "SECURITY"), "{w:?}");
    assert!(own_findings(&base, &snap(&[("shop/views.py", &new)])).is_empty());
    // A security check whose interface changed stays a meaning hazard, labelled as a security check.
    let auth = "def check_permission(user, perm):\n    if perm not in user.perms:\n        raise PermissionError(perm)\n";
    let base = snap(&[("app/auth.py", auth)]);
    let mut a = base.clone(); a.insert("app/auth.py".into(), auth.replace("(user, perm)", "(user, perm, scope)").into_bytes());
    let mut b = base.clone(); b.insert("app/x.py".into(), b"from app.auth import check_permission\n\n\ndef go(u):\n    check_permission(u, 'p')\n".to_vec());
    let w = analyze(&base, ("a", &a), ("b", &b));
    assert!(w.iter().any(|x| x.kind == "HAZARD" && x.severity.as_deref() == Some("high") && x.security.is_some()), "{w:?}");
}

#[test]
fn security_list_entries_taken_off_need_approval() {
    let base = snap(&[(".gitta-security", "# ours\nbilling.apply_limits\napp/auth/*\n")]);
    assert!(!own_findings(&base, &snap(&[(".gitta-security", "app/auth/*\n")])).is_empty());
    assert!(!own_findings(&base, &snap(&[])).is_empty());
    assert!(own_findings(&base, &snap(&[(".gitta-security", "billing.apply_limits\napp/auth/*\nshop.auth.*\n")])).is_empty());
}

/// Common real-world forms of Python login and permission decorators: removing one is a weakening, an edit to the
/// body that keeps it is not.
#[test]
fn python_access_decorator_forms() {
    let forms = [
        ("@token_auth.login_required", "login check"),
        ("@auth.login_required", "login check"),
        ("@auth.login_required(role=\"admin\")", "login check"),
        ("@login_required(login_url=\"/accounts/login/\")", "login check"),
        ("@fresh_login_required", "login check"),
        ("@permission_required(\"shop.export_orders\", raise_exception=True)", "permission check"),
        ("@user_passes_test(lambda u: u.is_staff)", "user check"),
        ("@staff_member_required", "staff check"),
        ("@permission_classes([IsAuthenticated])", "permission check"),
    ];
    for (deco, what) in forms {
        let v = format!("from shop.auth import auth\n\n\n@api_view([\"GET\"])\n{deco}\ndef export_orders(request):\n    return list_orders(request)\n\n\ndef list_orders(request):\n    return []\n");
        let base = snap(&[("shop/views.py", &v)]);
        let gone = snap(&[("shop/views.py", &v.replace(&format!("{deco}\n"), ""))]);
        let f = own_findings(&base, &gone);
        assert!(f.iter().any(|d| d.contains(what)), "{deco}: {f:?}");
        let kept = snap(&[("shop/views.py", &v.replace("    return list_orders(request)\n", "    rows = list_orders(request)\n    return rows\n"))]);
        assert!(own_findings(&base, &kept).is_empty(), "{deco}: unexpected {:?}", own_findings(&base, &kept));
    }
    // FastAPI: the login dependency removed from a route.
    let v = "from fastapi import Depends\n\n\ndef export_orders(user=Depends(get_current_user)):\n    return []\n";
    let base = snap(&[("api/routes.py", v)]);
    let gone = snap(&[("api/routes.py", &v.replace("user=Depends(get_current_user)", ""))]);
    assert!(own_findings(&base, &gone).iter().any(|d| d.contains("login dependency")));
    // Plain code that only mentions a name like it is not a control.
    let v = "def login_required_count(rows):\n    return len(rows)\n";
    let base = snap(&[("shop/stats.py", v)]);
    let new = snap(&[("shop/stats.py", &v.replace("len(rows)", "len(rows) + 0"))]);
    assert!(own_findings(&base, &new).is_empty());
}
