#![cfg_attr(test, feature(test))]
extern crate piston_meta;
extern crate rand;
extern crate range;
extern crate read_color;
extern crate read_token;
#[cfg(feature = "http")]
extern crate reqwest;
#[macro_use]
extern crate lazy_static;

use std::any::Any;
use std::fmt;
use std::thread::JoinHandle;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use range::Range;
use piston_meta::MetaData;

pub mod ast;
pub mod runtime;
pub mod lifetime;
pub mod intrinsics;
pub mod prelude;
pub mod embed;
pub mod ty;
pub mod link;
pub mod macros;
pub mod vec4;
pub mod write;

mod grab;

pub use runtime::Runtime;
pub use prelude::{Lt, Prelude, Dfn};
pub use ty::Type;
pub use link::Link;
pub use vec4::Vec4;

/// A common error message when there is no value on the stack.
pub const TINVOTS: &'static str = "There is no value on the stack";

pub type Array = Arc<Vec<Variable>>;
pub type Object = Arc<HashMap<Arc<String>, Variable>>;
pub type RustObject = Arc<Mutex<Any>>;

#[derive(Debug, Clone)]
pub struct Error {
    pub message: Variable,
    // Extra information to help debug error.
    // Stores error messages for all `?` operators.
    pub trace: Vec<String>,
}

#[derive(Clone)]
pub struct Thread {
    pub handle: Option<Arc<Mutex<JoinHandle<Result<Variable, String>>>>>,
}

impl Thread {
    pub fn new(handle: JoinHandle<Result<Variable, String>>) -> Thread {
        Thread {
            handle: Some(Arc::new(Mutex::new(handle)))
        }
    }

    /// Removes the thread handle from the stack.
    /// This is to prevent an extra reference when resolving the variable.
    pub fn invalidate_handle(
        rt: &mut Runtime,
        var: Variable
    ) -> Result<JoinHandle<Result<Variable, String>>, String> {
        use std::error::Error;

        let thread = match var {
            Variable::Ref(ind) => {
                use std::mem::replace;

                match replace(&mut rt.stack[ind], Variable::Thread(Thread { handle: None })) {
                    Variable::Thread(th) => th,
                    x => return Err(rt.expected(&x, "Thread"))
                }
            }
            Variable::Thread(thread) => thread,
            x => return Err(rt.expected(&x, "Thread"))
        };
        let handle = match thread.handle {
            None => return Err("The Thread has already been invalidated".into()),
            Some(thread) => thread
        };
        let mutex = try!(Arc::try_unwrap(handle).map_err(|_|
            format!("{}\nCan not access Thread because there is \
            more than one reference to it", rt.stack_trace())));
        mutex.into_inner().map_err(|err|
            format!("{}\nCan not lock Thread mutex:\n{}", rt.stack_trace(), err.description()))
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "thread")
    }
}

/// Prevents unsafe references from being accessed outside library.
#[derive(Debug, Clone)]
pub struct UnsafeRef(*mut Variable);

#[derive(Clone)]
pub struct ClosureEnvironment {
    pub module: Arc<Module>,
    pub relative: usize,
}

impl fmt::Debug for ClosureEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ClosureEnvironment")
    }
}

#[derive(Debug, Clone)]
pub enum Variable {
    Ref(usize),
    Return,
    Bool(bool, Option<Box<Vec<Variable>>>),
    F64(f64, Option<Box<Vec<Variable>>>),
    Vec4([f32; 4]),
    Text(Arc<String>),
    Array(Array),
    Object(Object),
    Link(Box<Link>),
    UnsafeRef(UnsafeRef),
    RustObject(RustObject),
    Option(Option<Box<Variable>>),
    Result(Result<Box<Variable>, Box<Error>>),
    Thread(Thread),
    // Stores closure AST, relative function index.
    Closure(Arc<ast::Closure>, Box<ClosureEnvironment>),
    In(Arc<Mutex<::std::sync::mpsc::Receiver<Variable>>>),
}

/// This is requires because `UnsafeRef(*mut Variable)` can not be sent across threads.
/// The lack of `UnsafeRef` variant when sending across threads is guaranteed at language level.
/// The interior of `UnsafeRef` can not be accessed outside this library.
unsafe impl Send for Variable {}

impl Variable {
    pub fn f64(val: f64) -> Variable {
        Variable::F64(val, None)
    }

    pub fn bool(val: bool) -> Variable {
        Variable::Bool(val, None)
    }

    fn deep_clone(&self, stack: &Vec<Variable>) -> Variable {
        use Variable::*;

        match *self {
            F64(_, _) => self.clone(),
            Vec4(_) => self.clone(),
            Return => self.clone(),
            Bool(_, _) => self.clone(),
            Text(_) => self.clone(),
            Object(ref obj) => {
                let mut res = obj.clone();
                for (_, val) in Arc::make_mut(&mut res) {
                    *val = val.deep_clone(stack);
                }
                Object(res)
            }
            Array(ref arr) => {
                let mut res = arr.clone();
                for it in Arc::make_mut(&mut res) {
                    *it = it.deep_clone(stack);
                }
                Array(res)
            }
            Link(_) => self.clone(),
            Ref(ind) => {
                stack[ind].deep_clone(stack)
            }
            UnsafeRef(_) => panic!("Unsafe reference can not be cloned"),
            RustObject(_) => self.clone(),
            Option(None) => Variable::Option(None),
            // `some(x)` always uses deep clone, so it does not contain references.
            Option(Some(ref v)) => Option(Some(v.clone())),
            // `ok(x)` always uses deep clone, so it does not contain references.
            Result(Ok(ref ok)) => Result(Ok(ok.clone())),
            // `err(x)` always uses deep clone, so it does not contain references.
            Result(Err(ref err)) => Result(Err(err.clone())),
            Thread(_) => self.clone(),
            Closure(_, _) => self.clone(),
            In(_) => self.clone(),
        }
    }
}

impl PartialEq for Variable {
    fn eq(&self, other: &Variable) -> bool {
        match (self, other) {
            (&Variable::Return, _) => false,
            (&Variable::Bool(a, _), &Variable::Bool(b, _)) => a == b,
            (&Variable::F64(a, _), &Variable::F64(b, _)) => a == b,
            (&Variable::Text(ref a), &Variable::Text(ref b)) => a == b,
            (&Variable::Object(ref a), &Variable::Object(ref b)) => a == b,
            (&Variable::Array(ref a), &Variable::Array(ref b)) => a == b,
            (&Variable::Ref(_), _) => false,
            (&Variable::UnsafeRef(_), _) => false,
            (&Variable::RustObject(_), _) => false,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum FnIndex {
    None,
    Intrinsic(usize),
    /// Relative to function you call from.
    Loaded(isize),
    ExternalVoid(FnExternalRef),
    ExternalReturn(FnExternalRef),
}

/// Used to store direct reference to external function.
#[derive(Copy)]
pub struct FnExternalRef(pub fn(&mut Runtime) -> Result<(), String>);

impl Clone for FnExternalRef {
    fn clone(&self) -> FnExternalRef {
        *self
    }
}

impl fmt::Debug for FnExternalRef {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "FnExternalRef")
    }
}

pub struct FnExternal {
    pub name: Arc<String>,
    pub f: fn(&mut Runtime) -> Result<(), String>,
    pub p: Dfn,
}

impl Clone for FnExternal {
    fn clone(&self) -> FnExternal {
        FnExternal {
            name: self.name.clone(),
            f: self.f,
            p: self.p.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Module {
    pub functions: Vec<ast::Function>,
    pub ext_prelude: Vec<FnExternal>,
    pub intrinsics: Arc<HashMap<Arc<String>, usize>>,
}

impl Module {
    pub fn new() -> Module {
        Module::new_intrinsics(Arc::new(Prelude::new_intrinsics().functions))
    }

    pub fn new_intrinsics(intrinsics: Arc<HashMap<Arc<String>, usize>>) -> Module {
        Module {
            functions: vec![],
            ext_prelude: vec![],
            intrinsics: intrinsics,
        }
    }

    pub fn register(&mut self, function: ast::Function) {
        self.functions.push(function);
    }

    /// Find function relative another function index.
    pub fn find_function(&self, name: &Arc<String>, relative: usize) -> FnIndex {
        for (i, f) in self.functions.iter().enumerate().rev() {
            if &f.name == name {
                return FnIndex::Loaded(i as isize - relative as isize);
            }
        }
        for f in self.ext_prelude.iter().rev() {
            if &f.name == name {
                return if f.p.returns() {
                    FnIndex::ExternalReturn(FnExternalRef(f.f))
                } else {
                    FnIndex::ExternalVoid(FnExternalRef(f.f))
                };
            }
        }
        match self.intrinsics.get(name) {
            None => FnIndex::None,
            Some(&ind) => FnIndex::Intrinsic(ind)
        }
    }

    pub fn error(&self, range: Range, msg: &str, rt: &Runtime) -> String {
        self.error_fnindex(range, msg, rt.call_stack.last().unwrap().index)
    }

    pub fn error_fnindex(&self, range: Range, msg: &str, fnindex: usize) -> String {
        let source = &self.functions[fnindex].source;
        self.error_source(range, msg, source)
    }

    pub fn error_source(&self, range: Range, msg: &str, source: &Arc<String>) -> String {
        use piston_meta::ParseErrorHandler;

        let mut w: Vec<u8> = vec![];
        ParseErrorHandler::new(source)
            .write_msg(&mut w, range, &format!("{}", msg))
            .unwrap();
        String::from_utf8(w).unwrap()
    }

    /// Adds a new extended prelude function.
    pub fn add(
        &mut self,
        name: Arc<String>,
        f: fn(&mut Runtime) -> Result<(), String>,
        prelude_function: Dfn
    ) {
        self.ext_prelude.push(FnExternal {
            name: name.clone(),
            f: f,
            p: prelude_function,
        });
    }
}

/// Runs a program using a source file.
pub fn run(source: &str) -> Result<(), String> {
    let mut module = Module::new_intrinsics(Arc::new(Prelude::new_intrinsics().functions));
    try!(load(source, &mut module));
    let mut runtime = runtime::Runtime::new();
    try!(runtime.run(&Arc::new(module)));
    Ok(())
}

/// Runs a program from a string.
pub fn run_str(source: &str, d: Arc<String>) -> Result<(), String> {
    let mut module = Module::new_intrinsics(Arc::new(Prelude::new_intrinsics().functions));
    try!(load_str(source, d, &mut module));
    let mut runtime = runtime::Runtime::new();
    try!(runtime.run(&Arc::new(module)));
    Ok(())
}

/// Used to call specific functions with arguments.
pub struct Call {
    args: Vec<Variable>,
    name: Arc<String>,
}

impl Call {
    /// Creates a new call.
    pub fn new(name: &str) -> Call {
        Call {
            args: vec![],
            name: Arc::new(name.into())
        }
    }

    /// Push value to argument list.
    pub fn arg<T: embed::PushVariable>(mut self, val: T) -> Self {
        self.args.push(val.push_var());
        self
    }

    /// Push Vec4 to argument list.
    pub fn vec4<T: embed::ConvertVec4>(mut self, val: T) -> Self {
        self.args.push(Variable::Vec4(val.to()));
        self
    }

    /// Push Rust object to argument list.
    pub fn rust<T: 'static>(mut self, val: T) -> Self {
        self.args.push(Variable::RustObject(Arc::new(Mutex::new(val)) as RustObject));
        self
    }

    /// Run call without any return value.
    pub fn run(&self, runtime: &mut Runtime, module: &Arc<Module>) -> Result<(), String> {
        runtime.call_str(&self.name, &self.args, module)
    }

    /// Run call with return value.
    pub fn run_ret<T: embed::PopVariable>(&self, runtime: &mut Runtime, module: &Arc<Module>) -> Result<T, String> {
        let val = runtime.call_str_ret(&self.name, &self.args, module)?;
        T::pop_var(runtime, runtime.resolve(&val))
    }

    /// Convert return value to a Vec4 convertible type.
    pub fn run_vec4<T: embed::ConvertVec4>(&self, runtime: &mut Runtime, module: &Arc<Module>) -> Result<T, String> {
        let val = runtime.call_str_ret(&self.name, &self.args, module)?;
        match runtime.resolve(&val) {
            &Variable::Vec4(val) => Ok(T::from(val)),
            x => Err(runtime.expected(x, "vec4"))
        }
    }
}

/// Loads source from file.
pub fn load(source: &str, module: &mut Module) -> Result<(), String> {
    use std::fs::File;
    use std::io::Read;

    let mut data_file = try!(File::open(source).map_err(|err|
        format!("Could not open `{}`, {}", source, err)));
    let mut data = Arc::new(String::new());
    data_file.read_to_string(Arc::make_mut(&mut data)).unwrap();
    load_str(source, data, module)
}

/// Loads a source from string.
///
/// - source - The name of source file
/// - d - The data of source file
/// - module - The module to load the source
pub fn load_str(source: &str, d: Arc<String>, module: &mut Module) -> Result<(), String> {
    use std::thread;
    use piston_meta::{parse_errstr, syntax_errstr, Syntax};

    lazy_static! {
        static ref SYNTAX_RULES: Result<Syntax, String> = {
            let syntax = include_str!("../assets/syntax.txt");
            syntax_errstr(syntax)
        };
    }

    let syntax_rules = try!(SYNTAX_RULES.as_ref()
        .map_err(|err| err.clone()));

    let mut data = vec![];
    try!(parse_errstr(syntax_rules, &d, &mut data).map_err(
        |err| format!("In `{}:`\n{}", source, err)
    ));

    let check_data = data.clone();
    let prelude = Arc::new(Prelude::from_module(module));

    // Do lifetime checking in parallel directly on meta data.
    let handle = thread::spawn(move || {
        let check_data = check_data;
        lifetime::check(&check_data, &prelude)
    });

    // Convert to AST.
    let mut ignored = vec![];
    let conv_res = ast::convert(Arc::new(source.into()), d.clone(), &data, &mut ignored, module);

    // Check that lifetime checking succeeded.
    match handle.join().unwrap() {
        Ok(refined_rets) => {
            for (name, ty) in &refined_rets {
                if let FnIndex::Loaded(f_index) = module.find_function(name, 0) {
                    let f = &mut module.functions[f_index as usize];
                    f.ret = ty.clone();
                }
            }
        }
        Err(err_msg) => {
            use std::io::Write;
            use piston_meta::ParseErrorHandler;

            let (range, msg) = err_msg.decouple();

            let mut buf: Vec<u8> = vec![];
            writeln!(&mut buf, "In `{}`:\n", source).unwrap();
            ParseErrorHandler::new(&d)
                .write_msg(&mut buf, range, &msg)
                .unwrap();
            return Err(String::from_utf8(buf).unwrap())
        }
    }

    check_ignored_meta_data(&conv_res, source, &d, &data, &ignored)
}

/// Loads a source from meta data.
/// Assumes the source passes the lifetime checker.
pub fn load_meta(
    source: &str,
    d: Arc<String>,
    data: &[Range<MetaData>],
    module: &mut Module
) -> Result<(), String> {
    // Convert to AST.
    let mut ignored = vec![];
    let conv_res = ast::convert(Arc::new(source.into()), d.clone(), &data, &mut ignored, module);

    check_ignored_meta_data(&conv_res, source, &d, data, &ignored)
}

fn check_ignored_meta_data(
    conv_res: &Result<(), ()>,
    source: &str,
    d: &Arc<String>,
    data: &[Range<MetaData>],
    ignored: &[Range],
) -> Result<(), String> {
    use piston_meta::json;

    if ignored.len() > 0 || conv_res.is_err() {
        use std::io::Write;
        use piston_meta::ParseErrorHandler;

        let mut buf: Vec<u8> = vec![];
        if ignored.len() > 0 {
            writeln!(&mut buf, "Some meta data was ignored in the syntax").unwrap();
            writeln!(&mut buf, "START IGNORED").unwrap();
            json::write(&mut buf, &data[ignored[0].iter()]).unwrap();
            writeln!(&mut buf, "END IGNORED").unwrap();

            writeln!(&mut buf, "In `{}`:\n", source).unwrap();
            ParseErrorHandler::new(&d)
                .write_msg(&mut buf,
                           data[ignored[0].iter()][0].range(),
                           "Could not understand this")
                .unwrap();
        }
        if let &Err(()) = conv_res {
            writeln!(&mut buf, "Conversion error").unwrap();
        }
        return Err(String::from_utf8(buf).unwrap());
    }

    Ok(())
}

/// Reports and error to standard output.
pub fn error(res: Result<(), String>) -> bool {
    match res {
        Err(err) => {
            println!("");
            println!(" --- ERROR --- ");
            println!("{}", err);
            true
        }
        Ok(()) => false
    }
}

#[cfg(test)]
mod tests {
    extern crate test;

    use super::run;
    use self::test::Bencher;

    #[test]
    fn variable_size() {
        use std::mem::size_of;
        use std::sync::Arc;
        use super::*;

        /*
        Ref(usize),
        Return,
        Bool(bool, Option<Box<Vec<Variable>>>),
        F64(f64, Option<Box<Vec<Variable>>>),
        Vec4([f32; 4]),
        Text(Arc<String>),
        Array(Array),
        Object(Object),
        Link(Box<Link>),
        UnsafeRef(UnsafeRef),
        RustObject(RustObject),
        Option(Option<Box<Variable>>),
        Result(Result<Box<Variable>, Box<Error>>),
        Thread(Thread),
        */

        println!("Link {}", size_of::<Box<Link>>());
        println!("[f32; 4] {}", size_of::<[f32; 4]>());
        println!("Result {}", size_of::<Result<Box<Variable>, Box<Error>>>());
        println!("Thread {}", size_of::<Thread>());
        println!("Secret {}", size_of::<Option<Box<Vec<Variable>>>>());
        println!("Text {}", size_of::<Arc<String>>());
        println!("Array {}", size_of::<Array>());
        println!("Object {}", size_of::<Object>());
        assert_eq!(size_of::<Variable>(), 24);
    }

    fn run_bench(source: &str) {
        run(source).unwrap_or_else(|err| panic!("{}", err));
    }

    #[bench]
    fn bench_add(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/add.dyon"));
    }

    #[bench]
    fn bench_add_n(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/add_n.dyon"));
    }

    #[bench]
    fn bench_sum(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/sum.dyon"));
    }

    #[bench]
    fn bench_main(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/main.dyon"));
    }

    #[bench]
    fn bench_array(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/array.dyon"));
    }

    #[bench]
    fn bench_object(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/object.dyon"));
    }

    #[bench]
    fn bench_call(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/call.dyon"));
    }

    #[bench]
    fn bench_n_body(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/n_body.dyon"));
    }

    #[bench]
    fn bench_len(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/len.dyon"));
    }

    #[bench]
    fn bench_min_fn(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/min_fn.dyon"));
    }

    #[bench]
    fn bench_min(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/min.dyon"));
    }

    #[bench]
    fn bench_primes(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/primes.dyon"));
    }

    #[bench]
    fn bench_primes_trad(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/primes_trad.dyon"));
    }

    #[bench]
    fn bench_threads_no_go(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/threads_no_go.dyon"));
    }

    #[bench]
    fn bench_threads_go(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/threads_go.dyon"));
    }

    #[bench]
    fn bench_push_array(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/push_array.dyon"));
    }

    #[bench]
    fn bench_push_link(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/push_link.dyon"));
    }

    #[bench]
    fn bench_push_link_for(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/push_link_for.dyon"));
    }

    #[bench]
    fn bench_push_link_go(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/push_link_go.dyon"));
    }

    #[bench]
    fn bench_push_str(b: &mut Bencher) {
        b.iter(|| run_bench("source/bench/push_str.dyon"));
    }
}
