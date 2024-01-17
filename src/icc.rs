// This is the intelligent controller interface
use rust_icc::*;
use std::ptr;

pub struct IccClient {
    handle: *mut icc_context,
}

impl IccClient {
    pub unsafe fn new() -> IccClient {
        let mut handle: *mut icc_context = ptr::null_mut();
        icc_init(
            icc_log_level_ICC_LOG_DEBUG,
            icc_client_type_ICC_TYPE_JOBMON,
            &mut handle,
        );
        IccClient { handle }
    }

    pub unsafe fn test(&mut self, value: u8) -> i32 {
        let mut retcode = 0;
        icc_rpc_test(
            self.handle,
            value,
            icc_client_type_ICC_TYPE_JOBMON,
            &mut retcode,
        );
        retcode
    }
}
