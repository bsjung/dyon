
fn main() {
    fs := functions()
    n := len(fs)
    // type := "intrinsic"
    type := "external"
    // type := "loaded"
    for i := 0; i < n; i += 1 {
        if fs[i].type != type { continue }
        print(function: fs[i])
    }
    // println(fs)

    say_hello()
    homer := homer()
    println(homer())
    homer = age(homer)
    println(homer)
}
