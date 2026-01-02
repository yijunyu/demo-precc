typedef enum
{
    HLF_8 = 0
    , HLF_AT
    , HLF_D
    , HLF_E
    , HLF_H
    , HLF_I
    , HLF_L
    , HLF_M
    , HLF_CM
    , HLF_N
    , HLF_R
    , HLF_S
    , HLF_SNC
    , HLF_C
    , HLF_T
    , HLF_V
    , HLF_VNC
    , HLF_W
    , HLF_WM
    , HLF_FL
    , HLF_FC
    , HLF_ADD
    , HLF_CHD
    , HLF_DED
    , HLF_TXD
    , HLF_CONCEAL
    , HLF_SC
    , HLF_SPB
    , HLF_SPC
    , HLF_SPR
    , HLF_SPL
    , HLF_PNI
    , HLF_PSI
    , HLF_PSB
    , HLF_PST
    , HLF_TP
    , HLF_TPS
    , HLF_TPF
    , HLF_CUC
    , HLF_CUL
    , HLF_MC
    , HLF_COUNT
} hlf_T;

    void
sign_list_placed(rbuf)
    buf_T *rbuf;
{
    buf_T *buf;
    signlist_T *p;
    char lbuf[8192];
    msg_puts_title((char_u *)(((char *)("\n--- Signs ---"))));
    msg_putchar('\n');
    if (rbuf == ((void *)0))
 buf = firstbuf;
    else
 buf = rbuf;
    while (buf != ((void *)0))
    {
 if (buf->b_signlist != ((void *)0))
 {
     vim_snprintf(lbuf, 8192, ((char *)("Signs for %s:")), buf->b_fname);
     msg_puts_attr((char_u *)(lbuf), (highlight_attr[(int)(HLF_D)]));
     msg_putchar('\n');
 }
 for (p = buf->b_signlist; p != ((void *)0); p = p->next)
 {
     vim_snprintf(lbuf, 8192, ((char *)("    line=%ld  id=%d  name=%s")),
      (long)p->lnum, p->id, sign_typenr2name(p->typenr));
     msg_puts((char_u *)(lbuf));
     msg_putchar('\n');
 }
 if (rbuf != ((void *)0))
     break;
 buf = buf->b_next;
    }
}

