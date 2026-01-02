/* Bug11: Test case for enum with computed values and custom types */
typedef unsigned int VOS_UINT32;
typedef unsigned char VOS_UINT8;

/* Forward declare struct used in function signature */
typedef struct CBB_MSGCDC_TLV_TABLE_STRU {
    int dummy;
} CBB_MSGCDC_TLV_TABLE_STRU;

enum SMP_PACKAGE_ID_ENUM {
    SMP_PACKAGE_RRE = 0x30
};
typedef enum RRE_ERR_E {
    ERR_SER_ARG_ERROR = ((((((SMP_PACKAGE_RRE) << 8) | (1))) << 16) | (0x00FF & (0x0001))),
} RRE_ERR_ENUM;

VOS_UINT32 rre_TlvDecode(const CBB_MSGCDC_TLV_TABLE_STRU *pstTlvTable, VOS_UINT8 *pucTlvBuf, VOS_UINT32 ulTlvBufLen, VOS_UINT32 ulTrans,
                         VOS_UINT8 *pucStructBuf, VOS_UINT32 ulStructBufLen, VOS_UINT32 *pulBufLenBeforeTlv)
{
    {if((0L == pstTlvTable || 0L == pucStructBuf || 0L == pucTlvBuf || 0L == pulBufLenBeforeTlv)) {return (ERR_SER_ARG_ERROR);}}
    return 0;
}

