//! Rust bindings for [libccp](https://github.com/ccp-project/libccp).
//! This crate is useful for writing CCP datapaths in Rust.
//!
//! Users need to implement two traits: `Datapath` and `CongestionOps`.
//! `Datapath` implements not specific to a single connection.
//! `CongestionOps` implements connection-level events.

/// Bindgen-generated libccp bindings.
#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(unused)]
mod ccp;

use std::sync::{Arc, Mutex};

/// Datapath-wide functionality.
/// ```
/// struct Dp(std::os::unix::net::UnixDatagram);
/// impl libccp::DatapathOps for Dp {
///     fn send_msg(&mut self, msg: &[u8]) {
///         self.0.send(msg).unwrap_or_else(|_| (0));
///     }
/// }
/// ```
pub trait DatapathOps {
    /// How should libccp communicate with the CCP congestion control algorithm?
    /// An `impl Datapath` should contain some IPC strategy, and transmit `msg` via that.
    fn send_msg(&mut self, msg: &[u8]);

    /// How should libccp log messages?
    /// By default, silently drop them.
    fn log(&self, _level: ccp::ccp_log_level, _msg: &str) {}
}

struct DatapathObj(Box<dyn DatapathOps>);

/// Construct a datapath object.
///
/// # Example
///
/// ```rust,no_run
/// struct Dp;
/// impl libccp::DatapathOps for Dp {
///     fn send_msg(&mut self, msg: &[u8]) {
///         println!("sent message: {:?}", msg);
///     }
/// }
///
/// fn main() {
///   let dp = Dp;
///   libccp::DatapathBuilder::default().with_ops(dp).with_id(57).init();
/// }
/// ```
#[derive(Debug)]
pub struct DatapathBuilder<T> {
    id: u32,
    ops: T,
}

impl Default for DatapathBuilder<()> {
    fn default() -> Self {
        DatapathBuilder { id: 0, ops: () }
    }
}

impl<T> DatapathBuilder<T> {
    /// Set the `id` of this datapath libccp will use to identify itself to the CCP runtime.
    pub fn with_id(self, id: u32) -> Self {
        Self { id, ..self }
    }

    /// Specify datapath-specific functionality.
    ///
    /// To be useful, T1 should impl `DatapathOps`.
    pub fn with_ops<T1>(self, ops: T1) -> DatapathBuilder<T1> {
        DatapathBuilder { id: self.id, ops }
    }
}

impl<T: DatapathOps + 'static> DatapathBuilder<T> {
    pub fn init(self) -> Result<Datapath, LibccpError> {
        // need 2 levels of Box so we can avoid passing a fat pointer down
        let dp = Box::new(DatapathObj(Box::new(self.ops)));
        let conn_array = unsafe { libc::malloc(1024 * std::mem::size_of::<ccp::ccp_connection>()) };
        let mut dp = ccp::ccp_datapath {
            set_cwnd: Some(ccp::set_cwnd),
            set_rate_abs: Some(ccp::set_rate_abs),
            time_zero: time::precise_time_ns(),
            now: Some(ccp::now),
            since_usecs: Some(ccp::since_usecs),
            after_usecs: Some(ccp::after_usecs),
            send_msg: Some(ccp::send_msg),
            log: Some(ccp::log),
            max_connections: 1024,
            max_programs: 10,
            programs: std::ptr::null_mut(),
            ccp_active_connections: conn_array as *mut ccp::ccp_connection,
            fto_us: 1000,
            last_msg_sent: 0,
            _in_fallback: false,
            impl_: Box::into_raw(dp) as *mut std::os::raw::c_void,
        };

        let ok = unsafe { ccp::ccp_init(&mut dp, self.id) };
        let e: LibccpError = ok.into();
        let e: Result<(), LibccpError> = e.into();
        e.map(|_| Datapath(dp))
    }
}

/// Represents datapath functionality.
/// libccp state is freed when this is dropped.
pub struct Datapath(ccp::ccp_datapath);

unsafe impl Send for Datapath {}
unsafe impl Sync for Datapath {}

impl Datapath {
    /// Call `DatapathBuilder` with default id = 0.
    /// Uses datapath id 0.
    #[deprecated(since = "1.1.0", note = "Please use DatapathBuilder instead.")]
    pub fn init<T: DatapathOps + 'static>(dp: T) -> Result<Self, LibccpError> {
        DatapathBuilder::default().with_ops(dp).init()
    }

    /// When the datapath receives an IPC message from the congestion
    /// control algorithm, call this function to tell libccp about it.
    pub fn recv_msg(&self, msg: &mut [u8]) -> Result<(), LibccpError> {
        let dp = &self.0;
        let buf_len = msg.len();
        let ok = unsafe {
            ccp::ccp_read_msg(
                dp as *const ccp::ccp_datapath as *mut ccp::ccp_datapath,
                msg.as_mut_ptr() as *mut std::os::raw::c_char,
                buf_len as i32,
            )
        };

        let e: LibccpError = ok.into();
        e.into()
    }
}

impl Drop for Datapath {
    fn drop(&mut self) {
        let dp = &mut self.0;
        unsafe { ccp::ccp_free(dp as *mut ccp::ccp_datapath) }
    }
}

/// Implement this trait on the type that holds
/// per-connection state.
/// ```
/// struct Cn {
///     curr_cwnd: u32,
///     curr_rate: u32,
/// }
///
/// impl libccp::CongestionOps for Cn {
///     fn set_cwnd(&mut self, cwnd: u32) {
///         self.curr_cwnd = cwnd;
///     }
///
///     fn set_rate_abs(&mut self, rate: u32) {
///         self.curr_rate = rate;
///     }
/// }
/// ```
pub trait CongestionOps {
    fn set_cwnd(&mut self, cwnd: u32);
    fn set_rate_abs(&mut self, rate: u32);
}

// safety: these methods are non-public, and Connection remembers the right `T` as its type
// parameter.
impl dyn CongestionOps {
    fn downcast<T: CongestionOps>(&self) -> &T {
        unsafe { &*(self as *const Self as *const T) }
    }

    fn downcast_mut<T: CongestionOps>(&mut self) -> &mut T {
        unsafe { &mut *(self as *mut Self as *mut T) }
    }
}

struct ConnectionObj(Box<dyn CongestionOps>);

macro_rules! setters {
    ( $s:ident => $($x: ident : $t: ty),+ ) => {
        paste::item! { impl $s { $(
            pub fn [<with_ $x>] (mut self, val: $t) -> Self { (self.0).$x = val; self }
        )*
	} }
    };
}

mod flow_info;
mod primitives;

pub use crate::flow_info::FlowInfo;
pub use crate::primitives::Primitives;

/// An individual Connection.
/// Connections cannot outlive the `Datapath` they belong to, since they contain
/// a pointer to memory that is freed when `Datapath` is dropped.
/// So, their lifetime is `'dp` from the `&'dp Datapath`.
///
/// You can regain access to the `impl CongestionOps` by dereferencing `Connection`
/// ```
/// struct Dp();
/// impl libccp::DatapathOps for Dp {
///     fn send_msg(&mut self, _: &[u8]) { /* ___ */ }
/// }
/// struct Cn(u32);
/// impl libccp::CongestionOps for Cn {
///     fn set_cwnd(&mut self, cwnd: u32) { /* ___ */ }   
///     fn set_rate_abs(&mut self, cwnd: u32) { /* ___ */ }   
/// }
/// fn main() {
///     let d = libccp::Datapath::init(Dp()).unwrap();
///     let mut c = libccp::Connection::start(&d, Cn(0), libccp::FlowInfo::default()).unwrap();
///     c.load_primitives(libccp::Primitives::default());
///     c.0 = 1;
/// }
/// ```
pub struct Connection<'dp, T: CongestionOps + 'static>(
    Arc<Mutex<*mut ccp::ccp_connection>>,
    Box<ConnectionObj>,
    &'dp Datapath,
    std::marker::PhantomData<T>,
);

impl<'dp, T: CongestionOps + 'static> Connection<'dp, T> {
    /// Call this function when a connection starts.
    /// `conn: impl CongestionOps` represents per-connection state,
    /// and how to mutate it in response to changing congestion windows
    /// or rates.
    pub fn start(token: &'dp Datapath, conn: T, flow_info: FlowInfo) -> Result<Self, LibccpError> {
        let conn_obj = Box::new(ConnectionObj(Box::new(conn) as Box<dyn CongestionOps>));
        let ops_raw_pointer = Box::into_raw(conn_obj);
        let conn_obj = unsafe { Box::from_raw(ops_raw_pointer.clone()) };
        let dp = &token.0;
        let conn = unsafe {
            ccp::ccp_connection_start(
                dp as *const ccp::ccp_datapath as *mut ccp::ccp_datapath,
                Box::into_raw(conn_obj) as *mut std::os::raw::c_void,
                &mut flow_info.get_dp_info(),
            )
        };

        if conn.is_null() {
            return Err(LibccpError::OtherError("Could not initialize connection"));
        }

        Ok(Connection(
            Arc::new(Mutex::new(conn)),
            unsafe { Box::from_raw(ops_raw_pointer) },
            token,
            Default::default(),
        ))
    }

    /// Inform libccp of new measurements.
    pub fn load_primitives(&mut self, prims: Primitives) {
        let mut ptr = self.0.lock().expect("Lock ccp_connection");
        unsafe {
            (**ptr).prims = prims.0;
        }
    }

    pub fn primitives(&self, _token: &Datapath) -> Primitives {
        let ptr = self.0.lock().expect("Lock ccp_connection");
        let pr = unsafe { &(**ptr).prims };
        pr.into()
    }

    /// Tell libccp to invoke. This will run the congestion control's datapath program,
    /// and potentially result in calls to the `CongestionOps` callbacks.
    /// Therefore, ensure that when you call this function, you are not holding locks that
    /// the `CongestionOps` functionality tries to acquire - this will deadlock.
    pub fn invoke(&mut self) -> Result<(), LibccpError> {
        let ptr = self.0.lock().expect("Lock ccp_connection");
        let ok = unsafe { ccp::ccp_invoke(*ptr) };

        let e: LibccpError = ok.into();
        e.into()
    }
}

unsafe impl<'dp, T: CongestionOps> Send for Connection<'dp, T> {}

impl<'dp, T: CongestionOps> Drop for Connection<'dp, T> {
    fn drop(&mut self) {
        let ptr = self.0.lock().expect("Lock ccp_connection");
        unsafe {
            let index = (**ptr).index;
            let dp = (**ptr).datapath;
            ccp::ccp_connection_free(dp, index);
        }
    }
}

impl<'dp, T: CongestionOps> std::ops::Deref for Connection<'dp, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        let y = &*(self.1);
        y.0.downcast::<T>()
    }
}

impl<'dp, T: CongestionOps> std::ops::DerefMut for Connection<'dp, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let y = &mut *(self.1);
        y.0.downcast_mut::<T>()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LibccpError {
    OtherError(&'static str),
    LibccpOk,
    LibccpMissingArg,
    LibccpNullArg,
    LibccpBufsizeNegative,
    LibccpBufsizeTooSmall,
    LibccpMsgTooLong,
    LibccpWriteInvalidHeaderType,
    LibccpReadInvalidHeaderType,
    LibccpReadInvalidOp,
    LibccpReadRegNotAllowed,
    LibccpReadInvalidReturnReg,
    LibccpReadInvalidLeftReg,
    LibccpReadInvalidRightReg,
    LibccpInstallTypeMismatch,
    LibccpInstallTooManyExpr,
    LibccpInstallTooManyInstr,
    LibccpUpdateTypeMismatch,
    LibccpUpdateTooMany,
    LibccpUpdateInvalidRegType,
    LibccpChangeTypeMismatch,
    LibccpChangeTooMany,
    LibccpUnknownConnection,
    LibccpCreatePending,
    LibccpConnectionNotInitialized,
    LibccpProgTableFull,
    LibccpProgNotFound,
    LibccpAddIntOverflow,
    LibccpDivByZero,
    LibccpMulIntOverflow,
    LibccpSubIntUnderflow,
    LibccpPrivIsNull,
    LibccpProgIsNull,
    LibccpFallbackTimedOut,
}

impl From<i32> for LibccpError {
    fn from(e: i32) -> Self {
        match e {
            x if x == ccp::LIBCCP_OK as _ => LibccpError::LibccpOk,
            ccp::LIBCCP_MISSING_ARG => LibccpError::LibccpMissingArg,
            ccp::LIBCCP_NULL_ARG => LibccpError::LibccpNullArg,
            ccp::LIBCCP_BUFSIZE_NEGATIVE => LibccpError::LibccpBufsizeNegative,
            ccp::LIBCCP_BUFSIZE_TOO_SMALL => LibccpError::LibccpBufsizeTooSmall,
            ccp::LIBCCP_MSG_TOO_LONG => LibccpError::LibccpMsgTooLong,
            ccp::LIBCCP_WRITE_INVALID_HEADER_TYPE => LibccpError::LibccpWriteInvalidHeaderType,
            ccp::LIBCCP_READ_INVALID_HEADER_TYPE => LibccpError::LibccpReadInvalidHeaderType,
            ccp::LIBCCP_READ_INVALID_OP => LibccpError::LibccpReadInvalidOp,
            ccp::LIBCCP_READ_REG_NOT_ALLOWED => LibccpError::LibccpReadRegNotAllowed,
            ccp::LIBCCP_READ_INVALID_RETURN_REG => LibccpError::LibccpReadInvalidReturnReg,
            ccp::LIBCCP_READ_INVALID_LEFT_REG => LibccpError::LibccpReadInvalidLeftReg,
            ccp::LIBCCP_READ_INVALID_RIGHT_REG => LibccpError::LibccpReadInvalidRightReg,
            ccp::LIBCCP_INSTALL_TYPE_MISMATCH => LibccpError::LibccpInstallTypeMismatch,
            ccp::LIBCCP_INSTALL_TOO_MANY_EXPR => LibccpError::LibccpInstallTooManyExpr,
            ccp::LIBCCP_INSTALL_TOO_MANY_INSTR => LibccpError::LibccpInstallTooManyInstr,
            ccp::LIBCCP_UPDATE_TYPE_MISMATCH => LibccpError::LibccpUpdateTypeMismatch,
            ccp::LIBCCP_UPDATE_TOO_MANY => LibccpError::LibccpUpdateTooMany,
            ccp::LIBCCP_UPDATE_INVALID_REG_TYPE => LibccpError::LibccpUpdateInvalidRegType,
            ccp::LIBCCP_CHANGE_TYPE_MISMATCH => LibccpError::LibccpChangeTypeMismatch,
            ccp::LIBCCP_CHANGE_TOO_MANY => LibccpError::LibccpChangeTooMany,
            ccp::LIBCCP_UNKNOWN_CONNECTION => LibccpError::LibccpUnknownConnection,
            ccp::LIBCCP_CREATE_PENDING => LibccpError::LibccpCreatePending,
            ccp::LIBCCP_CONNECTION_NOT_INITIALIZED => LibccpError::LibccpConnectionNotInitialized,
            ccp::LIBCCP_PROG_TABLE_FULL => LibccpError::LibccpProgTableFull,
            ccp::LIBCCP_PROG_NOT_FOUND => LibccpError::LibccpProgNotFound,
            ccp::LIBCCP_ADD_INT_OVERFLOW => LibccpError::LibccpAddIntOverflow,
            ccp::LIBCCP_DIV_BY_ZERO => LibccpError::LibccpDivByZero,
            ccp::LIBCCP_MUL_INT_OVERFLOW => LibccpError::LibccpMulIntOverflow,
            ccp::LIBCCP_SUB_INT_UNDERFLOW => LibccpError::LibccpSubIntUnderflow,
            ccp::LIBCCP_PRIV_IS_NULL => LibccpError::LibccpPrivIsNull,
            ccp::LIBCCP_PROG_IS_NULL => LibccpError::LibccpProgIsNull,
            ccp::LIBCCP_FALLBACK_TIMED_OUT => LibccpError::LibccpFallbackTimedOut,
            _ => LibccpError::OtherError("unknown error code"),
        }
    }
}

impl Into<Result<(), Self>> for LibccpError {
    fn into(self) -> Result<(), Self> {
        match self {
            LibccpError::LibccpOk => Ok(()),
            x => Err(x),
        }
    }
}

impl std::fmt::Display for LibccpError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        match self {
            LibccpError::OtherError(msg) => write!(f, "{}", msg),
            LibccpError::LibccpOk => write!(f, "Ok."),
            LibccpError::LibccpMissingArg => write!(f, "Missing Argument"),
            LibccpError::LibccpNullArg => write!(f, "Null Argument"),
            LibccpError::LibccpBufsizeNegative => write!(f, "Provided buffer size was negative"),
            LibccpError::LibccpBufsizeTooSmall => write!(f, "Provided buffer size was too small"),
            LibccpError::LibccpMsgTooLong => write!(f, "Message too long"),
            LibccpError::LibccpWriteInvalidHeaderType => {
                write!(f, "Tried to write invalid header type")
            }
            LibccpError::LibccpReadInvalidHeaderType => {
                write!(f, "Tried to read invalid header type")
            }
            LibccpError::LibccpReadInvalidOp => write!(f, "Read Invalid opcode"),
            LibccpError::LibccpReadRegNotAllowed => write!(f, "Read register not allowed here"),
            LibccpError::LibccpReadInvalidReturnReg => write!(f, "Invalid return register"),
            LibccpError::LibccpReadInvalidLeftReg => write!(f, "Invalid lvalue"),
            LibccpError::LibccpReadInvalidRightReg => write!(f, "Invalid rvalue"),
            LibccpError::LibccpInstallTypeMismatch => write!(f, "Install: Type mismatch"),
            LibccpError::LibccpInstallTooManyExpr => write!(f, "Install: Too many expressions"),
            LibccpError::LibccpInstallTooManyInstr => write!(f, "Install: Too many instructions"),
            LibccpError::LibccpUpdateTypeMismatch => write!(f, "Update: type mismatch"),
            LibccpError::LibccpUpdateTooMany => write!(f, "Update: too many values"),
            LibccpError::LibccpUpdateInvalidRegType => write!(f, "Update: invalid register type"),
            LibccpError::LibccpChangeTypeMismatch => write!(f, "Change: type mismatch"),
            LibccpError::LibccpChangeTooMany => write!(f, "Change: too many values"),
            LibccpError::LibccpUnknownConnection => write!(f, "Unknown connection"),
            LibccpError::LibccpCreatePending => write!(f, "Create was pending"),
            LibccpError::LibccpConnectionNotInitialized => write!(f, "Connection not initialized"),
            LibccpError::LibccpProgTableFull => write!(f, "Program table full"),
            LibccpError::LibccpProgNotFound => write!(f, "Program not found"),
            LibccpError::LibccpAddIntOverflow => write!(f, "Integer overflow (addition)"),
            LibccpError::LibccpDivByZero => write!(f, "Divide by zero"),
            LibccpError::LibccpMulIntOverflow => write!(f, "Integer overflow (multiplication)"),
            LibccpError::LibccpSubIntUnderflow => write!(f, "Integer underflow (subtraction)"),
            LibccpError::LibccpPrivIsNull => write!(f, "Private state is null"),
            LibccpError::LibccpProgIsNull => write!(f, "Program is null"),
            LibccpError::LibccpFallbackTimedOut => write!(f, "Fallback timed out"),
        }
    }
}

impl std::error::Error for LibccpError {}

#[cfg(test)]
mod tests {
    use super::*;

    // mock implementation
    struct Dp {
        expected_msgs: Vec<Option<Vec<u8>>>,
    }

    impl DatapathOps for Dp {
        fn send_msg(&mut self, msg: &[u8]) {
            if self.expected_msgs.is_empty() {
                return;
            }

            let expected_send_msg: Vec<u8> = self.expected_msgs.pop().unwrap().unwrap();
            println!(
                "this message: {:?}, remaining messsages: {:?}",
                expected_send_msg, self.expected_msgs
            );
            println!("got msg: {:?}", msg);
            assert_eq!(msg, &expected_send_msg[..msg.len()]);
        }
    }

    struct Cn {
        curr_cwnd: u32,
        curr_rate: u32,
    }

    impl CongestionOps for Cn {
        fn set_cwnd(&mut self, cwnd: u32) {
            self.curr_cwnd = cwnd;
            println!("cwnd = {:?}", self.curr_cwnd);
        }

        fn set_rate_abs(&mut self, rate: u32) {
            self.curr_rate = rate;
            println!("rate = {:?}", self.curr_rate);
        }
    }

    fn make_dp(mut expected_msgs: Vec<Option<Vec<u8>>>) -> Datapath {
        #[rustfmt::skip]
        let rdy_msg = vec![
            0x05,0x00,
            0x0c,0x00,
            0x00,0x00,0x00,0x00,
            0xaa,0x00,0x00,0x00,
        ];
        expected_msgs.push(Some(rdy_msg));
        let dp = Dp { expected_msgs };
        DatapathBuilder::default()
            .with_ops(dp)
            .with_id(0xaa)
            .init()
            .unwrap()
    }

    fn make_conn(d: &Datapath) -> Connection<Cn> {
        let cn = Cn {
            curr_cwnd: 19,
            curr_rate: 89,
        };

        let fi = FlowInfo::default()
            .with_init_cwnd(100)
            .with_mss(10)
            .with_four_tuple(1, 2, 3, 4);

        let c = Connection::start(d, cn, fi).unwrap();
        assert_eq!(c.curr_cwnd, 19);
        c
    }

    use lazy_static::lazy_static;
    use std::sync::Mutex;

    lazy_static! {
        static ref TEST_MUTEX: Mutex<()> = Mutex::new(());
    }

    #[test]
    fn primitives() {
        let _l = TEST_MUTEX.lock().unwrap();

        let prims_prog_uid = 5;

        #[rustfmt::skip]
        let mut crmsg = vec![ // create msg
                0x00,0x00,
                0x60,0x00,
                0x01,0x00,0x00,0x00,
                0x64,0x00,0x00,0x00,
                0x0a,0x00,0x00,0x00,
                0x01,0x00,0x00,0x00,
                0x02,0x00,0x00,0x00,
                0x03,0x00,0x00,0x00,
                0x04,0x00,0x00,0x00,
            ];
        crmsg.extend(&[0u8; 64]);

        #[rustfmt::skip]
        let dp = make_dp(vec![
            Some(vec![ //  close msg
                0x01, 0,
                16, 0,
                0x01, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ]),
            Some(vec![ // report msg
                0x01, 0x00,                                     // type 
                0x18, 0x00,                                     // len
                0x01, 0x00, 0x00, 0x00,                         // sockid
                prims_prog_uid, 0x00, 0x00, 0x00,               // program_uid
                0x01, 0x00, 0x00, 0x00,                         // num_fields
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // fields
            ]),
            Some(crmsg),
        ]);

        let mut c = make_conn(&dp);

        #[rustfmt::skip]
        let mut install_prims = vec![
            2, 0,                                           // INSTALL                                                     
            116, 0,                                         // length = 0x74 = 116
            1, 0, 0, 0,                                     // sock_id = 1                                                 
            prims_prog_uid, 0, 0, 0,                        // program_uid
            1, 0, 0, 0,                                     // num_events = 1                                              
            5, 0, 0, 0,                                     // num_instrs = 5                                              
            1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, // event { flag-idx=1, num-flag=1, body-idx=2, num-body=3 }
            2, 5, 0, 0, 0, 0, 5, 0, 0, 0, 0, 1, 0, 0, 0, 0, // (def (volatile Report.foo 0))
            1, 2, 0, 0, 0, 0, 2, 0, 0, 0, 0, 1, 1, 0, 0, 0, // (when true
            0, 7, 0, 0, 0, 0, 5, 0, 0, 0, 0, 4, 0, 0, 0, 0, //      ----------------(+ Report.foo Ack.bytes_acked)
            1, 5, 0, 0, 0, 0, 5, 0, 0, 0, 0, 7, 0, 0, 0, 0, //     (bind Report.foo ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^)
            1, 2, 2, 0, 0, 0, 2, 2, 0, 0, 0, 1, 1, 0, 0, 0, //     (bind __shouldReport true)
        ];

        #[rustfmt::skip]
        let mut changeprog_msg = vec![
            4, 0,                                           // type
            12, 0,                                          // length 
            1, 0, 0, 0,                                     // sock id
            prims_prog_uid, 0, 0, 0,                        // program_uid
            0, 0, 0, 0,                                     // extra fields
        ];

        dp.recv_msg(&mut install_prims).unwrap();
        dp.recv_msg(&mut changeprog_msg).unwrap();

        c.load_primitives(
            Primitives::default()
                .with_bytes_acked(1)
                .with_rtt_sample_us(100),
        );
        c.invoke().unwrap();
    }

    #[test]
    fn primitives_multiple() {
        let _l = TEST_MUTEX.lock().unwrap();

        let prims_prog_uid = 5;

        #[rustfmt::skip]
        let close =
            Some(vec![ //  close msg
                0x01, 0,
                16, 0,
                0x01, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ]);

        #[rustfmt::skip]
        let report =
            Some(vec![ // report msg
                0x01, 0x00,                                     // type 
                0x18, 0x00,                                     // len
                0x01, 0x00, 0x00, 0x00,                         // sockid
                prims_prog_uid, 0x00, 0x00, 0x00,               // program_uid
                0x01, 0x00, 0x00, 0x00,                         // num_fields
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // fields
            ]);

        #[rustfmt::skip]
        let mut create =
            Some(vec![ // create msg
                0x00,0x00,
                0x60,0x00,
                0x01,0x00,0x00,0x00,
                0x64,0x00,0x00,0x00,
                0x0a,0x00,0x00,0x00,
                0x01,0x00,0x00,0x00,
                0x02,0x00,0x00,0x00,
                0x03,0x00,0x00,0x00,
                0x04,0x00,0x00,0x00,
            ]);
        create.as_mut().unwrap().extend(&[0u8; 64]);

        let mut msgs = vec![close];

        // 4 reports
        msgs.push(report.clone());
        msgs.push(report.clone());
        msgs.push(report.clone());
        msgs.push(report);

        msgs.push(create);

        let dp = make_dp(msgs);
        let mut c = make_conn(&dp);

        #[rustfmt::skip]
        let mut install_prims = vec![
            2, 0,                                           // INSTALL                                                     
            116, 0,                                         // length = 0x74 = 116
            1, 0, 0, 0,                                     // sock_id = 1                                                 
            prims_prog_uid, 0, 0, 0,                        // program_uid
            1, 0, 0, 0,                                     // num_events = 1                                              
            5, 0, 0, 0,                                     // num_instrs = 5                                              
            1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, // event { flag-idx=1, num-flag=1, body-idx=2, num-body=3 }
            2, 5, 0, 0, 0, 0, 5, 0, 0, 0, 0, 1, 0, 0, 0, 0, // (def (volatile Report.foo 0))
            1, 2, 0, 0, 0, 0, 2, 0, 0, 0, 0, 1, 1, 0, 0, 0, // (when true
            0, 7, 0, 0, 0, 0, 5, 0, 0, 0, 0, 4, 0, 0, 0, 0, //      ----------------(+ Report.foo Ack.bytes_acked)
            1, 5, 0, 0, 0, 0, 5, 0, 0, 0, 0, 7, 0, 0, 0, 0, //     (bind Report.foo ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^)
            1, 2, 2, 0, 0, 0, 2, 2, 0, 0, 0, 1, 1, 0, 0, 0, //     (bind __shouldReport true)
        ];

        #[rustfmt::skip]
        let mut changeprog_msg = vec![
            4, 0,                                           // type
            12, 0,                                          // length 
            1, 0, 0, 0,                                     // sock id
            prims_prog_uid, 0, 0, 0,                        // program_uid
            0, 0, 0, 0,                                     // extra fields
        ];

        dp.recv_msg(&mut install_prims).unwrap();
        dp.recv_msg(&mut changeprog_msg).unwrap();

        // 4 invokes
        for _ in 0..4 {
            c.load_primitives(
                Primitives::default()
                    .with_bytes_acked(1)
                    .with_rtt_sample_us(100),
            );
            c.invoke().unwrap();
        }
    }

    #[test]
    fn basic() {
        let _l = TEST_MUTEX.lock().unwrap();

        let basic_prog_uid = 4;

        #[rustfmt::skip]
        let mut crmsg = vec![ // create msg
                0x00,0x00,
                0x60,0x00,
                0x01,0x00,0x00,0x00,
                0x64,0x00,0x00,0x00,
                0x0a,0x00,0x00,0x00,
                0x01,0x00,0x00,0x00,
                0x02,0x00,0x00,0x00,
                0x03,0x00,0x00,0x00,
                0x04,0x00,0x00,0x00,
            ];
        crmsg.extend(&[0u8; 64]);

        #[rustfmt::skip]
        let dp = make_dp(vec![
            Some(vec![ //  close msg
                0x01, 0,
                16, 0,
                0x01, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ]),
            Some(vec![ // report msg
                0x01, 0x00,                                     // type 
                0x18, 0x00,                                     // len
                0x01, 0x00, 0x00, 0x00,                         // sockid
                basic_prog_uid, 0x00, 0x00, 0x00,               // program_uid
                0x01, 0x00, 0x00, 0x00,                         // num_fields
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // fields
            ]),
            Some(crmsg),
        ]);

        let mut c = make_conn(&dp);

        #[rustfmt::skip]
        let mut install_basic = vec![
            2, 0,                                           // INSTALL                                                     
            116, 0,                                         // length = 0x74 = 116
            1, 0, 0, 0,                                     // sock_id = 1                                                 
            basic_prog_uid, 0, 0, 0,                        // program_uid
            1, 0, 0, 0,                                     // num_events = 1                                              
            5, 0, 0, 0,                                     // num_instrs = 5                                              
            1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, // event { flag-idx=1, num-flag=1, body-idx=2, num-body=3 }
            2, 5, 0, 0, 0, 0, 5, 0, 0, 0, 0, 1, 0, 0, 0, 0, // (def (volatile Report.foo 0))
            1, 2, 0, 0, 0, 0, 2, 0, 0, 0, 0, 1, 1, 0, 0, 0, // (when true
            0, 7, 0, 0, 0, 0, 5, 0, 0, 0, 0, 1, 1, 0, 0, 0, //      ----------------(+ Report.foo 1)
            1, 5, 0, 0, 0, 0, 5, 0, 0, 0, 0, 7, 0, 0, 0, 0, //     (bind Report.foo ^^^^^^^^^^^^^^^^)
            1, 2, 2, 0, 0, 0, 2, 2, 0, 0, 0, 1, 1, 0, 0, 0, //     (bind __shouldReport true)
        ];

        #[rustfmt::skip]
        let mut changeprog_msg = vec![
            4, 0,                                           // type
            12, 0,                                          // length 
            1, 0, 0, 0,                                     // sock id
            basic_prog_uid, 0, 0, 0,                        // program_uid
            0, 0, 0, 0,                                     // extra fields
        ];

        dp.recv_msg(&mut install_basic).unwrap();
        dp.recv_msg(&mut changeprog_msg).unwrap();
        c.invoke().unwrap();
    }
}
