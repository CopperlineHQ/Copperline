/* Classic Enforcer-style lawbreaker: touch protected low memory and
 * survive. Under MuForce each access must raise a reported hit and the
 * program must keep running (read supplied, write absorbed). */
volatile long *zero = (volatile long *)0;
int main(void)
{
    long v = *zero;               /* read hit at 0 */
    *(volatile long *)0x40 = v;   /* write hit at 0x40 */
    v = *(volatile long *)0x30;   /* second read hit */
    return 0;
}
